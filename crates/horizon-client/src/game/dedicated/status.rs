//! What `horizon-server` asks the dedicated server on its control endpoint (`--control-socket`, docs/server.md
//! section 2): `server-status` answers with the tick rate, the tick times `/tick query` reports, the players,
//! the loaded chunks, the Java jars the loader started and the last NeoForge FML preflight; `console <line>`
//! runs a console command and answers with the lines it logged.
use super::{JavaCompatibilityReport, ServerState, capture_log, fml, loader_name, role_name};
use crate::game::Game;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

/// The window the tick rate is measured over.
const TPS_WINDOW: Duration = Duration::from_secs(5);
/// The most jars `server-status` lists one by one; the counts stay whole.
const MAX_STATUS_JARS: usize = 8;
/// The most payload directions one jar lists.
const MAX_STATUS_ROWS: usize = 12;
/// The most mod ids the `fml` section lists; FML's count stays whole.
const MAX_FML_IDS: usize = 16;
/// The most characters of a preflight failure the `fml` section repeats.
const MAX_FML_ERROR_CHARS: usize = 200;

/// The `java` section of `server-status`: which explicitly selected jars the
/// dedicated loader started, each one's loader, role and entrypoint kind, how
/// its registrations landed, and whether any was refused. Sorted by jar path
/// and bounded, so two servers that started the same jars answer the same.
pub(super) fn java_section(reports: &[JavaCompatibilityReport]) -> Value {
    let mut sorted: Vec<&JavaCompatibilityReport> = reports.iter().collect();
    sorted.sort_by(|a, b| a.jar.cmp(&b.jar));
    let mut directions: BTreeSet<&str> = BTreeSet::new();
    for report in &sorted {
        directions.extend(report.payload_directions.iter().map(String::as_str));
    }
    let count = |of: fn(&JavaCompatibilityReport) -> usize| {
        sorted.iter().map(|report| of(report)).sum::<usize>()
    };
    let entries: Vec<Value> = sorted
        .iter()
        .take(MAX_STATUS_JARS)
        .map(|report| {
            json!({
                "jar": report.jar.display().to_string(),
                "loader": report.loader.map_or("unknown", loader_name),
                "role": role_name(report.role),
                "entrypoint": if report.entrypoint_kind.is_empty() {
                    "none"
                } else {
                    &report.entrypoint_kind
                },
                "typed": report.typed_registrations,
                "opaque": report.opaque_registrations.len(),
                "promoted": report.promoted.len(),
                "payload": report
                    .payload_directions
                    .iter()
                    .take(MAX_STATUS_ROWS)
                    .collect::<Vec<_>>(),
                "rejected": report.client_side_only_rejected,
            })
        })
        .collect();
    json!({
        "jars": sorted.len(),
        "listed": entries.len(),
        "rejected": sorted.iter().filter(|r| r.client_side_only_rejected).count(),
        "typed": count(|r| r.typed_registrations),
        "opaque": count(|r| r.opaque_registrations.len()),
        "promoted": count(|r| r.promoted.len()),
        "payload_directions": directions,
        "entries": entries,
    })
}

/// The `fml` section of `server-status`: the last opt-in FancyModLoader
/// preflight — its role, the jars offered to it, FML's own mod count and ids,
/// why it failed and whether the environment asked for it to be strict. The
/// keys are fixed: a server that never ran one answers `not-run` with empty
/// fields, which is not the same as a failed preflight.
pub(super) fn fml_section(report: Option<&fml::FmlReport>, strict: bool) -> Value {
    let Some(report) = report else {
        return json!({
            "status": "not-run",
            "role": null,
            "jars": 0,
            "mods": null,
            "ids": [],
            "error": null,
            "strict": strict,
        });
    };
    let mut ids = report.mod_ids.clone();
    ids.sort();
    ids.dedup();
    ids.truncate(MAX_FML_IDS);
    json!({
        "status": if report.succeeded() { "ok" } else { "failed" },
        "role": role_name(report.role),
        "jars": report.jars,
        "mods": report.mods,
        "ids": ids,
        "error": report.failure.as_deref().map(bounded_error),
        "strict": strict,
    })
}

/// A failure reason clipped for the status reply: control characters become
/// spaces and only [`MAX_FML_ERROR_CHARS`] characters are kept.
fn bounded_error(failure: &str) -> String {
    let mut bounded: String = failure
        .chars()
        .take(MAX_FML_ERROR_CHARS)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if failure.chars().count() > MAX_FML_ERROR_CHARS {
        bounded.push_str("...");
    }
    bounded
}

impl ServerState {
    /// One pass of the server loop ended.
    pub(super) fn ticked(&mut self, at: Instant) {
        self.ticks.push_back(at);
        while self
            .ticks
            .front()
            .is_some_and(|t| at.duration_since(*t) > TPS_WINDOW)
        {
            self.ticks.pop_front();
        }
    }
    /// Ticks per second over the last few seconds.
    fn tps(&self) -> f64 {
        match (self.ticks.front(), self.ticks.back()) {
            (Some(first), Some(last)) if self.ticks.len() > 1 => {
                (self.ticks.len() - 1) as f64 / last.duration_since(*first).as_secs_f64().max(1e-9)
            }
            _ => 0.0,
        }
    }
}

impl Game<'_> {
    /// Answers the requests waiting on the control endpoint.
    pub(super) fn poll_server_control(&mut self, state: &mut ServerState) {
        while let Some(request) = self.control.as_ref().and_then(|c| c.try_recv()) {
            let reply = match request.line.trim().split_once(' ') {
                _ if request.line.trim() == "server-status" => self.server_status(state),
                Some(("console", line)) => {
                    // Command feedback is logged as system chat; the reply carries the text alone.
                    let output: Vec<String> = capture_log(|| self.console_command(state, line))
                        .into_iter()
                        .map(|l| {
                            l.strip_prefix("System chat: ")
                                .map_or(l.clone(), str::to_owned)
                        })
                        .collect();
                    json!({ "output": output })
                }
                _ => json!({ "error": format!("unknown request {:?}", request.line) }),
            };
            let _ = request.reply.send(reply.to_string());
        }
    }

    fn server_status(&self, state: &ServerState) -> Value {
        let millis = |nanos: u64| nanos as f64 / 1e6;
        let samples = self.clock.sorted_samples();
        // The percentiles `/tick query` prints (TickCommand.percentileToString).
        let at = |q: f64| {
            let i = ((samples.len() as f64 * q) as usize).min(samples.len().saturating_sub(1));
            samples.get(i).copied().map_or(0.0, millis)
        };
        let (players, max_players) = self.lan.as_ref().map_or((Vec::new(), 0), |h| {
            let players = h
                .hub
                .guests
                .iter()
                .map(|g| {
                    json!({
                        "name": g.name,
                        "uuid": horizon_protocol::crypto::uuid_string(g.uuid),
                        "address": g.address.ip().to_string(),
                        "ping_ms": g.latency.load(Ordering::Relaxed),
                    })
                })
                .collect();
            (players, h.hub.lan.shared.max_players())
        });
        json!({
            "tps": state.tps(),
            "target_tps": self.clock.rate(),
            "mspt": {
                "average": millis(self.clock.average_nanos()),
                "p50": at(0.5),
                "p95": at(0.95),
                "p99": at(0.99),
            },
            "players": players,
            "max_players": max_players,
            "chunks": self.world.chunks.len(),
            "paused": state.paused,
            "java": java_section(&state.java),
            "fml": fml_section(fml::last_report().as_ref(), fml::strict_requested()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::execution_role::ExecutionRole;

    fn report(
        jars: usize,
        mods: Option<usize>,
        mod_ids: &[&str],
        failure: Option<&str>,
    ) -> fml::FmlReport {
        fml::FmlReport {
            role: ExecutionRole::DedicatedServer,
            jars,
            mods,
            mod_ids: mod_ids.iter().map(|id| (*id).to_owned()).collect(),
            failure: failure.map(str::to_owned),
        }
    }

    #[test]
    fn a_completed_preflight_says_ok_with_its_counts() {
        let report = report(2, Some(3), &["beta", "alpha", "alpha"], None);
        let section = fml_section(Some(&report), false);
        assert_eq!(
            section,
            json!({
                "status": "ok",
                "role": "dedicated-server",
                "jars": 2,
                "mods": 3,
                "ids": ["alpha", "beta"],
                "error": null,
                "strict": false,
            })
        );
        // Fixed keys, so a status reader never has to guess at a shape.
        assert_eq!(
            section.to_string(),
            "{\"error\":null,\"ids\":[\"alpha\",\"beta\"],\"jars\":2,\"mods\":3,\
             \"role\":\"dedicated-server\",\"status\":\"ok\",\"strict\":false}"
        );
    }

    #[test]
    fn a_failed_preflight_says_failed_and_bounds_its_ids_and_error() {
        let ids: Vec<String> = (0..MAX_FML_IDS + 8)
            .rev()
            .map(|i| format!("m{i:02}"))
            .collect();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let failure = format!("\t{}\nsecond line", "x".repeat(MAX_FML_ERROR_CHARS + 40));
        let report = report(1, None, &ids, Some(&failure));
        let section = fml_section(Some(&report), true);
        assert_eq!(section["status"], "failed");
        assert_eq!(section["role"], "dedicated-server");
        assert_eq!(section["jars"], 1);
        // No count from FML: null, not a made-up zero.
        assert_eq!(section["mods"], json!(null));
        assert_eq!(section["strict"], true);
        // Sorted, deduplicated and bounded whatever order the report kept.
        let listed = section["ids"].as_array().unwrap();
        assert_eq!(listed.len(), MAX_FML_IDS);
        assert_eq!(listed[0], "m00");
        assert_eq!(listed[MAX_FML_IDS - 1], "m15");
        let error = section["error"].as_str().unwrap();
        assert_eq!(error.chars().count(), MAX_FML_ERROR_CHARS + 3);
        assert!(error.starts_with(" xxx"), "{error}");
        assert!(error.ends_with("..."), "{error}");
        // A failure reason is one line: control characters become spaces.
        assert!(!error.chars().any(char::is_control), "{error}");
    }

    #[test]
    fn no_preflight_answers_not_run_with_empty_fields() {
        let section = fml_section(None, false);
        assert_eq!(
            section,
            json!({
                "status": "not-run",
                "role": null,
                "jars": 0,
                "mods": null,
                "ids": [],
                "error": null,
                "strict": false,
            })
        );
        assert_eq!(
            section.to_string(),
            "{\"error\":null,\"ids\":[],\"jars\":0,\"mods\":null,\"role\":null,\
             \"status\":\"not-run\",\"strict\":false}"
        );
        // The environment's strict request shows even when nothing ran.
        assert_eq!(fml_section(None, true)["strict"], true);
    }
}

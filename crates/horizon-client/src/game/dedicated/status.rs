//! What `horizon-server` asks the dedicated server on its control endpoint (`--control-socket`, docs/server.md
//! section 2): `server-status` answers with the tick rate, the tick times `/tick query` reports, the players,
//! the loaded chunks and the Java jars the loader started; `console <line>` runs a console command and answers
//! with the lines it logged.
use super::{JavaCompatibilityReport, ServerState, capture_log, loader_name, role_name};
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
        })
    }
}

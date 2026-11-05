//! What `horizon-server` asks the dedicated server on its control endpoint (`--control-socket`, docs/server.md
//! section 2): `server-status` answers with the tick rate, the tick times `/tick query` reports, the players,
//! the loaded chunks, the Java jars the loader started and the last NeoForge FML preflight; `console <line>`
//! runs a console command and answers with the lines it logged.
use super::{
    JavaCompatibilityReport, JavaEngine, ServerState, capture_log, fml, loader_name, role_name,
};
use crate::game::Game;
use crate::game::java_registry::{JavaRegistrySnapshot, PayloadSummary};
use horizon_client::mod_content::DeclarationSummary;
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
/// The most diagnostic rows (entrypoints and Mixin targets) one jar lists.
const MAX_STATUS_DIAGNOSTICS: usize = 12;
/// The most mod ids the `fml` section lists; FML's count stays whole.
const MAX_FML_IDS: usize = 16;
/// The most characters of a preflight failure the `fml` section repeats.
const MAX_FML_ERROR_CHARS: usize = 200;

/// `JavaEngine`'s labels as the summary lines print them. The enum's `name` is
/// private to `mods`; a test keeps this in step with it, like [`loader_name`].
pub(super) fn engine_name(engine: JavaEngine) -> &'static str {
    match engine {
        JavaEngine::Native => "native",
        JavaEngine::NativeQualified => "native-qualified",
        JavaEngine::Sidecar => "sidecar",
        JavaEngine::Refused => "refused",
    }
}

/// The engine labels `engines` counts, in the order it prints them.
const ENGINE_LABELS: [JavaEngine; 4] = [
    JavaEngine::Native,
    JavaEngine::NativeQualified,
    JavaEngine::Sidecar,
    JavaEngine::Refused,
];

/// What actually executed a jar, as opposed to how complete the result was:
/// a native result stays in process, the sidecar fallback pays for a JVM, and
/// a refused jar ran on nothing at all. `backends` counts every one.
fn backend_name(engine: JavaEngine) -> &'static str {
    match engine {
        JavaEngine::Native | JavaEngine::NativeQualified => "native",
        JavaEngine::Sidecar => "jvm",
        JavaEngine::Refused => "none",
    }
}

/// The backends `backends` counts, in the order it prints them.
const BACKEND_LABELS: [&str; 3] = ["native", "jvm", "none"];

/// The `java` section of `server-status`: which explicitly selected jars the
/// dedicated loader started, each one's engine, backend, loader, role and
/// entrypoint kind, how its registrations landed, and whether any was refused.
/// Each entry also carries the work the jar cost — executed entrypoints, native
/// host calls, listeners, Mixin planning and what the load weave did with those
/// plans — and the section totals those over every jar, so the bounded entry
/// list never hides the whole. Sorted by jar path and bounded, so two servers
/// that started the same jars answer the same.
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
    // Every engine label with its count over every jar, refused ones included:
    // the entries list is bounded, these counts are not.
    let engines: serde_json::Map<String, Value> = ENGINE_LABELS
        .iter()
        .map(|engine| {
            let count = sorted
                .iter()
                .filter(|report| report.engine() == *engine)
                .count();
            (engine_name(*engine).to_owned(), json!(count))
        })
        .collect();
    // The same jars by what ran them, not by how complete the run was: a
    // qualified native result still costs no JVM.
    let backends: serde_json::Map<String, Value> = BACKEND_LABELS
        .iter()
        .map(|backend| {
            let count = sorted
                .iter()
                .filter(|report| backend_name(report.engine()) == *backend)
                .count();
            ((*backend).to_owned(), json!(count))
        })
        .collect();
    let entries: Vec<Value> = sorted
        .iter()
        .take(MAX_STATUS_JARS)
        .map(|report| {
            json!({
                "jar": report.jar.display().to_string(),
                "id": report.id.as_deref(),
                "version": report.version.as_deref(),
                "engine": engine_name(report.engine()),
                "backend": backend_name(report.engine()),
                "loader": report.loader.map_or("unknown", loader_name),
                "role": role_name(report.role),
                "entrypoint": if report.entrypoint_kind.is_empty() {
                    "none"
                } else {
                    &report.entrypoint_kind
                },
                "declared_entrypoints": report
                    .declared_entrypoints
                    .iter()
                    .take(MAX_STATUS_DIAGNOSTICS)
                    .collect::<Vec<_>>(),
                "typed": report.typed_registrations,
                "opaque": report.opaque_registrations.len(),
                "promoted": report.promoted.len(),
                "executed": report.executed_entrypoints,
                "host_calls": report.host_calls,
                "listeners": report.listeners,
                "mixin_plans": report.mixin_plans,
                "mixin_unresolved": report.mixin_unresolved,
                "mixin_configs": report.mixin_configs,
                "mixin_config_errors": report.mixin_config_errors,
                // One bounded row per jar: the counts the load weave recorded
                // and the first reason it stayed out, cut like any failure.
                "mixin_woven": report.mixin_weave.woven,
                "mixin_skipped": report.mixin_weave.skipped,
                "mixin_skipped_reason": report
                    .mixin_weave
                    .reason
                    .as_deref()
                    .map(bounded_error),
                "mixin_targets": report
                    .mixin_targets
                    .iter()
                    .take(MAX_STATUS_DIAGNOSTICS)
                    .collect::<Vec<_>>(),
                "isolation_failure": report.isolation_failure.as_deref().map(bounded_error),
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
        "isolated": sorted.iter().filter(|r| r.isolation_failure.is_some()).count(),
        "engines": engines,
        "backends": backends,
        "typed": count(|r| r.typed_registrations),
        "opaque": count(|r| r.opaque_registrations.len()),
        "promoted": count(|r| r.promoted.len()),
        "executed": count(|r| r.executed_entrypoints),
        "host_calls": count(|r| r.host_calls),
        "listeners": count(|r| r.listeners),
        "mixin_plans": count(|r| r.mixin_plans),
        "mixin_unresolved": count(|r| r.mixin_unresolved),
        "mixin_configs": count(|r| r.mixin_configs),
        "mixin_config_errors": count(|r| r.mixin_config_errors),
        "mixin_woven": count(|r| r.mixin_weave.woven),
        "mixin_skipped": count(|r| r.mixin_weave.skipped),
        "payload_directions": directions,
        "entries": entries,
    })
}

/// The `declared` subtotal of the `java` section: what the typed registry
/// snapshot holds per registry kind, the distinct menu bindings and
/// block-entity blocks, the rows coalescing folded away (sorted by table, a
/// fixed set of tables), and the snapshot's own bounded compatibility lines.
/// The lines are [`JavaRegistrySnapshot::compatibility_lines`] verbatim, cut to
/// [`MAX_STATUS_ROWS`], so status and log never format the same counts twice.
/// Every key is fixed, so a server with no declarations answers zeros.
pub(super) fn declared_section(
    snapshot: &JavaRegistrySnapshot,
    declared: &DeclarationSummary,
    payloads: &PayloadSummary,
) -> Value {
    let summary = snapshot.summary();
    let mut lines = snapshot.compatibility_lines(declared, payloads);
    let truncated = lines.len() > MAX_STATUS_ROWS;
    lines.truncate(MAX_STATUS_ROWS);
    json!({
        "blocks": summary.blocks,
        "items": summary.items,
        "entities": summary.entities,
        "data_components": summary.data_components,
        "attachments": summary.attachments,
        "menus": summary.menus,
        "block_entities": summary.block_entities,
        "menu_bindings": summary.menu_bindings,
        "block_entity_blocks": summary.block_entity_blocks,
        "coalesced": snapshot.coalesced,
        "lines": lines,
        "lines_truncated": truncated,
    })
}

/// [`java_section`] plus a `declared` key at the end. The jar keys are
/// unchanged, so a status reader written before the key existed keeps
/// reading the same fields and ignores the new one.
pub(super) fn java_section_with_declared(
    reports: &[JavaCompatibilityReport],
    snapshot: &JavaRegistrySnapshot,
    declared: &DeclarationSummary,
    payloads: &PayloadSummary,
) -> Value {
    let mut section = java_section(reports);
    section["declared"] = declared_section(snapshot, declared, payloads);
    section
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
            "java": java_section_with_declared(
                &state.java,
                &crate::game::java_registry::current_snapshot(),
                &horizon_client::mod_content::declaration_summary(&self.options.java_content),
                &PayloadSummary::from_declarations(
                    &horizon_client::lan::payloads::declared_by_id(),
                ),
            ),
            "fml": fml_section(fml::last_report().as_ref(), fml::strict_requested()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::execution_role::ExecutionRole;
    use crate::game::mods::MixinWeave;

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

    /// A compatibility report built in memory with just the fields the status
    /// section reads, so the backend and workload fields are testable without
    /// compiling a jar or starting a loader.
    fn java_report(
        jar: &str,
        engine: JavaEngine,
        executed: usize,
        host_calls: usize,
        listeners: usize,
        mixin: (usize, usize, usize, usize),
    ) -> JavaCompatibilityReport {
        JavaCompatibilityReport {
            jar: std::path::PathBuf::from(jar),
            id: None,
            version: None,
            loader: None,
            declared_entrypoints: Vec::new(),
            entrypoint_kind: String::new(),
            executed_entrypoints: executed,
            host_calls,
            typed_registrations: 0,
            opaque_registrations: Vec::new(),
            promoted: Vec::new(),
            payload_directions: Vec::new(),
            listeners,
            mixin_plans: mixin.0,
            mixin_targets: Vec::new(),
            mixin_unresolved: mixin.1,
            mixin_configs: mixin.2,
            mixin_config_errors: mixin.3,
            mixin_weave: MixinWeave::default(),
            client_side_only_rejected: false,
            isolation_failure: None,
            role: ExecutionRole::DedicatedServer,
            engine: Some(engine),
        }
    }

    /// The backend answers what ran a jar, not how complete the result was: a
    /// qualified native run still pays for no JVM, and a refusal pays for
    /// nothing.
    #[test]
    fn every_engine_names_one_backend() {
        assert_eq!(backend_name(JavaEngine::Native), "native");
        assert_eq!(backend_name(JavaEngine::NativeQualified), "native");
        assert_eq!(backend_name(JavaEngine::Sidecar), "jvm");
        assert_eq!(backend_name(JavaEngine::Refused), "none");
    }

    #[test]
    fn java_section_counts_backends_and_the_work_each_jar_cost() {
        let reports = [
            java_report(
                "/mods/b-sidecar.jar",
                JavaEngine::Sidecar,
                2,
                40,
                3,
                (1, 2, 3, 0),
            ),
            java_report(
                "/mods/a-native.jar",
                JavaEngine::Native,
                1,
                7,
                1,
                (0, 0, 0, 0),
            ),
            java_report(
                "/mods/c-refused.jar",
                JavaEngine::Refused,
                0,
                0,
                0,
                (0, 0, 0, 0),
            ),
            java_report(
                "/mods/d-qualified.jar",
                JavaEngine::NativeQualified,
                1,
                5,
                0,
                (2, 1, 1, 1),
            ),
        ];
        let section = java_section(&reports);
        // Fixed keys: the three backends are always present, zero or not.
        assert_eq!(
            section["backends"],
            json!({"native": 2, "jvm": 1, "none": 1})
        );
        assert_eq!(
            section["engines"],
            json!({"native": 1, "native-qualified": 1, "sidecar": 1, "refused": 1})
        );
        // Totals cover every jar, including any the bounded entry list drops.
        assert_eq!(section["executed"], 4);
        assert_eq!(section["host_calls"], 52);
        assert_eq!(section["listeners"], 4);
        assert_eq!(section["mixin_plans"], 3);
        assert_eq!(section["mixin_unresolved"], 3);
        assert_eq!(section["mixin_configs"], 4);
        assert_eq!(section["mixin_config_errors"], 1);
        // Entries are sorted by jar and repeat their own backend and counters.
        let entries = section["entries"].as_array().unwrap();
        let jars: Vec<&str> = entries
            .iter()
            .map(|entry| entry["jar"].as_str().unwrap())
            .collect();
        assert_eq!(
            jars,
            [
                "/mods/a-native.jar",
                "/mods/b-sidecar.jar",
                "/mods/c-refused.jar",
                "/mods/d-qualified.jar"
            ]
        );
        assert_eq!(entries[0]["backend"], "native");
        assert_eq!(entries[0]["host_calls"], 7);
        assert_eq!(entries[1]["backend"], "jvm");
        assert_eq!(entries[1]["executed"], 2);
        assert_eq!(entries[1]["listeners"], 3);
        assert_eq!(entries[2]["backend"], "none");
        assert_eq!(entries[2]["executed"], 0);
        assert_eq!(entries[3]["backend"], "native");
        assert_eq!(entries[3]["engine"], "native-qualified");
        assert_eq!(entries[3]["mixin_unresolved"], 1);
        assert_eq!(entries[3]["mixin_config_errors"], 1);
    }

    /// The weave's own counts sit beside the planning counts, one bounded row
    /// per jar: what the load path applied, what it left out and the first
    /// reason, with totals that survive the entry bound like the other counts.
    /// The rows are additive, so a reader written before them keeps decoding
    /// the keys it knows.
    #[test]
    fn java_section_reports_the_load_weave_beside_the_planning_counts() {
        let mut woven = java_report(
            "/mods/a-woven.jar",
            JavaEngine::Native,
            1,
            6,
            0,
            (1, 0, 0, 0),
        );
        woven.mixin_weave = MixinWeave {
            woven: 1,
            skipped: 0,
            reason: None,
        };
        let mut skipped = java_report(
            "/mods/b-skipped.jar",
            JavaEngine::NativeQualified,
            1,
            5,
            0,
            (2, 0, 0, 0),
        );
        skipped.mixin_weave = MixinWeave {
            woven: 1,
            skipped: 1,
            reason: Some("cancellable HEAD demo/Target#other()V".into()),
        };
        let refused = java_report(
            "/mods/c-refused.jar",
            JavaEngine::Refused,
            0,
            0,
            0,
            (0, 0, 0, 0),
        );
        // A reason from a report that was not built here must still be cut to
        // the same bound as every other status row.
        let mut long = java_report(
            "/mods/d-long.jar",
            JavaEngine::NativeQualified,
            1,
            4,
            0,
            (2, 0, 0, 0),
        );
        long.mixin_weave = MixinWeave {
            woven: 0,
            skipped: 2,
            reason: Some("x".repeat(MAX_FML_ERROR_CHARS * 2)),
        };
        let reports = [
            skipped.clone(),
            woven.clone(),
            refused.clone(),
            long.clone(),
        ];
        let section = java_section(&reports);

        assert_eq!(section["mixin_woven"], 2);
        assert_eq!(section["mixin_skipped"], 3);
        let entries = section["entries"].as_array().unwrap();
        assert_eq!(entries[0]["jar"], "/mods/a-woven.jar");
        assert_eq!(entries[0]["mixin_woven"], 1);
        assert_eq!(entries[0]["mixin_skipped"], 0);
        assert_eq!(entries[0]["mixin_skipped_reason"], Value::Null);
        assert_eq!(entries[1]["jar"], "/mods/b-skipped.jar");
        assert_eq!(entries[1]["mixin_woven"], 1);
        assert_eq!(entries[1]["mixin_skipped"], 1);
        assert_eq!(
            entries[1]["mixin_skipped_reason"],
            "cancellable HEAD demo/Target#other()V"
        );
        // A jar the loader never wove claims neither work nor skips.
        assert_eq!(entries[2]["mixin_woven"], 0);
        assert_eq!(entries[2]["mixin_skipped"], 0);
        assert_eq!(entries[2]["mixin_skipped_reason"], Value::Null);
        assert_eq!(
            entries[3]["mixin_skipped_reason"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            MAX_FML_ERROR_CHARS + 3
        );

        // The wire keeps the old planning keys exactly as they were, so a
        // reader that only knows them still decodes its fields.
        assert_eq!(section["mixin_plans"], 5);
        assert_eq!(section["mixin_unresolved"], 0);
        let wire: Value = serde_json::from_str(&section.to_string()).unwrap();
        assert_eq!(wire["entries"][1]["engine"], "native-qualified");
        assert_eq!(wire["entries"][1]["mixin_skipped"], 1);
        let mut old = wire;
        let object = old.as_object_mut().unwrap();
        object.remove("mixin_woven");
        object.remove("mixin_skipped");
        for entry in object["entries"].as_array_mut().unwrap() {
            let entry = entry.as_object_mut().unwrap();
            entry.remove("mixin_woven");
            entry.remove("mixin_skipped");
            entry.remove("mixin_skipped_reason");
        }
        assert_eq!(old["entries"][1]["engine"], "native-qualified");
        assert_eq!(old["entries"][1]["mixin_plans"], 2);

        // Deterministic: the same reports give byte-identical JSON.
        assert_eq!(
            section.to_string(),
            java_section(&[skipped, woven, refused, long]).to_string()
        );
    }

    /// A refused jar keeps the report's refusal whatever its other fields say,
    /// so a backend reader never sees `native` for a jar that never ran.
    #[test]
    fn a_refused_jar_is_never_counted_as_native_work() {
        let mut refused = java_report(
            "/mods/refused.jar",
            JavaEngine::Refused,
            0,
            0,
            0,
            (0, 0, 0, 0),
        );
        refused.client_side_only_rejected = true;
        let section = java_section(&[refused]);
        assert_eq!(
            section["backends"],
            json!({"native": 0, "jvm": 0, "none": 1})
        );
        assert_eq!(section["entries"][0]["backend"], "none");
        assert_eq!(section["entries"][0]["engine"], "refused");
        assert_eq!(section["entries"][0]["executed"], 0);
        assert_eq!(section["host_calls"], 0);
        assert_eq!(section["rejected"], 1);
        assert_eq!(section["isolated"], 0);
    }

    #[test]
    fn java_section_keeps_identity_and_native_failure_diagnostics_bounded() {
        let mut report = java_report(
            "/mods/diagnostic.jar",
            JavaEngine::NativeQualified,
            1,
            2,
            0,
            (0, 0, 0, 0),
        );
        report.id = Some("diagnostic".to_owned());
        report.version = Some("1.2.3".to_owned());
        report.declared_entrypoints = (0..MAX_STATUS_DIAGNOSTICS + 2)
            .map(|i| format!("entrypoint-{i:02}"))
            .collect();
        report.mixin_targets = (0..MAX_STATUS_DIAGNOSTICS + 2)
            .map(|i| format!("Owner#{i:02}()V"))
            .collect();
        report.isolation_failure = Some("panic\nwhile invoking entrypoint".to_owned());

        let section = java_section(&[report]);
        assert_eq!(section["isolated"], 1);
        let entry = &section["entries"][0];
        assert_eq!(entry["id"], "diagnostic");
        assert_eq!(entry["version"], "1.2.3");
        assert_eq!(
            entry["declared_entrypoints"].as_array().unwrap().len(),
            MAX_STATUS_DIAGNOSTICS
        );
        assert_eq!(
            entry["mixin_targets"].as_array().unwrap().len(),
            MAX_STATUS_DIAGNOSTICS
        );
        assert_eq!(
            entry["isolation_failure"],
            "panic while invoking entrypoint"
        );
    }

    /// The entry list is bounded; the counts and the work totals are not, so a
    /// server with more jars than the listing still answers for all of them.
    #[test]
    fn java_section_totals_survive_the_entry_bound() {
        let reports: Vec<JavaCompatibilityReport> = (0..MAX_STATUS_JARS + 2)
            .map(|i| {
                java_report(
                    &format!("/mods/{i:02}.jar"),
                    JavaEngine::Native,
                    1,
                    2,
                    0,
                    (0, 0, 0, 0),
                )
            })
            .collect();
        let section = java_section(&reports);
        assert_eq!(section["jars"], json!(MAX_STATUS_JARS + 2));
        assert_eq!(section["listed"], json!(MAX_STATUS_JARS));
        assert_eq!(section["host_calls"], json!(2 * (MAX_STATUS_JARS + 2)));
        assert_eq!(section["backends"]["native"], json!(MAX_STATUS_JARS + 2));
        assert_eq!(
            section["entries"].as_array().unwrap().len(),
            MAX_STATUS_JARS
        );
    }

    /// A snapshot with a few ids per kind, built by hand: the status section
    /// reads counts, so no content root or process-wide registry is involved.
    fn snapshot_with(blocks: &[&str], menus: usize) -> JavaRegistrySnapshot {
        let strings = |ids: &[&str]| ids.iter().map(|id| (*id).to_owned()).collect();
        JavaRegistrySnapshot {
            blocks: strings(blocks),
            items: strings(&["ruby:gem"]),
            menus: (0..menus)
                .map(|i| horizon_mods::native::MenuDeclaration {
                    id: format!("ruby:menu{i}"),
                    title_key: "container.ruby.crate".into(),
                    slots: 27,
                    open_blocks: Vec::new(),
                })
                .collect(),
            coalesced: [("items".to_owned(), 2)].into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn declared_section_counts_each_registry_kind() {
        let snapshot = snapshot_with(&["ruby:ore", "ruby:block"], 1);
        let section = declared_section(&snapshot, &Default::default(), &Default::default());
        assert_eq!(section["blocks"], 2);
        assert_eq!(section["items"], 1);
        assert_eq!(section["menus"], 1);
        assert_eq!(section["entities"], 0);
        assert_eq!(section["coalesced"], json!({ "items": 2 }));
        // The lines are the snapshot's own, not a second formatting of them.
        let expected = snapshot.compatibility_lines(&Default::default(), &Default::default());
        let lines: Vec<String> = section["lines"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(lines, expected);
        assert_eq!(section["lines_truncated"], false);
        // Deterministic: the same inputs give byte-identical output.
        assert_eq!(
            section.to_string(),
            declared_section(&snapshot, &Default::default(), &Default::default()).to_string()
        );
    }

    #[test]
    fn an_empty_snapshot_answers_zeros_and_declared_none() {
        let section = declared_section(
            &Default::default(),
            &Default::default(),
            &Default::default(),
        );
        for key in [
            "blocks",
            "items",
            "entities",
            "data_components",
            "attachments",
            "menus",
            "block_entities",
            "menu_bindings",
            "block_entity_blocks",
        ] {
            assert_eq!(section[key], 0, "{key}");
        }
        assert_eq!(section["coalesced"], json!({}));
        assert!(
            section["lines"]
                .as_array()
                .unwrap()
                .iter()
                .any(|line| line.as_str().unwrap().ends_with("declared: none"))
        );
    }

    #[test]
    fn declared_lines_stay_within_the_row_bound() {
        // Many declared kinds and payload ids still print at most the cap.
        let declared = DeclarationSummary {
            blocks: 3,
            items: 3,
            ..Default::default()
        };
        let section = declared_section(&snapshot_with(&["a:b"], 0), &declared, &Default::default());
        assert!(section["lines"].as_array().unwrap().len() <= MAX_STATUS_ROWS);
    }

    /// The new key sits beside the old ones: removing it leaves exactly the
    /// section an older server answered, and an older reader's fields still
    /// parse from the serialized form.
    #[test]
    fn the_declared_key_does_not_disturb_the_old_java_section() {
        let reports = [java_report(
            "/mods/a.jar",
            JavaEngine::Native,
            1,
            2,
            3,
            (0, 0, 0, 0),
        )];
        let snapshot = snapshot_with(&["ruby:ore"], 0);
        let mut section = java_section_with_declared(
            &reports,
            &snapshot,
            &Default::default(),
            &Default::default(),
        );
        let wire = section.to_string();
        let parsed: Value = serde_json::from_str(&wire).unwrap();
        assert_eq!(parsed["jars"], 1);
        assert_eq!(parsed["entries"].as_array().unwrap().len(), 1);
        assert_eq!(parsed["declared"]["blocks"], 1);
        section.as_object_mut().unwrap().remove("declared");
        assert_eq!(section, java_section(&reports));
    }
}

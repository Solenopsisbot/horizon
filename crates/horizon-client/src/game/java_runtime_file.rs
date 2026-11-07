//! `java-runtime.json`: bounded per-jar diagnostics written under the game home for the launcher's runtime
//! panel, `tools/java_mod_server_ledger.py` and support tooling.
//!
//! One row per jar the batch was given: a jar that started carries its engine, the weave's counts, the plans
//! behind its skips (the first [`MAX_PLANS_PER_JAR`], loader-relevant ones first) and the skip counts by
//! reason; a jar the runner refused carries `engine = "refused"` and the runner's whole cause in `refusal`.
//! Every addition since schema 1 is optional on read, so the schema stays 1 and an older launcher still
//! reads what it knows.
//!
//! When the opt-in FML preflight ran (`game/fml.rs`), the file also carries its `fml` section, the same
//! answer `server-status` gives: a role started with no jar still writes the file for it, with no rows.
//!
//! The file never grows past [`MAX_BYTES`]: when the rows do not fit, the per-jar plan lists shrink first
//! (the counts stay whole), and only then are trailing rows dropped (`jars_total` keeps the whole count).
use horizon_native_java::{MixinSkipReason, SkippedMixinPlan};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fs, path::Path};

pub(crate) const SCHEMA_VERSION: u32 = 1;
/// The launcher refuses a larger file (`launcher/src/browser/java.rs`), so the writer stays under it.
const MAX_BYTES: usize = 256 * 1024;
/// Rows written at all.
const MAX_JARS: usize = 256;
/// Plans one jar lists; `skipped` and `skipped_by_reason` keep the whole count.
pub(crate) const MAX_PLANS_PER_JAR: usize = 64;
/// The longest refusal cause a row (or `server-status`) carries. No refusal the corpora produce comes near it
/// (the longest is a few hundred characters); it only stops a pathological jar's own strings from deciding how
/// large the file grows.
pub(crate) const MAX_CAUSE_CHARS: usize = 2048;
/// The longest class, method or detail string one plan row carries.
const MAX_NAME_CHARS: usize = 256;

/// One skipped mixin plan, as the launcher explains it: which mixin class wanted to change which target, with
/// what injector, and why the bounded weave left it out.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimePlan {
    /// The `@Mixin` class holding the callback (`com/example/mixin/LevelMixin`).
    pub(crate) mixin: String,
    /// The class the plan targets (`net/minecraft/world/level/Level`).
    pub(crate) target: String,
    /// The target method's name, empty for a plan that names none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub(crate) method: String,
    /// The plan's `@At` anchor (`HEAD`, `TAIL`, `INVOKE`...), or its annotation for a family that has none
    /// (`@ModifyArg`, `@Mixin-member`, an accessor).
    pub(crate) injector: String,
    /// The fixed skip label ([`MixinSkipReason::label`]): `target-outside-jar`, `unsupported-family`...
    pub(crate) reason: String,
    /// The bounded sentence a weaver or hook-mismatch skip carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) detail: Option<String>,
}

impl RuntimePlan {
    pub(crate) fn from_skipped(plan: &SkippedMixinPlan) -> Self {
        // A config-listed class with no plan names no target method; its `callback.owner` is the class.
        let method = if plan.reason == MixinSkipReason::ListedWithoutPlan {
            String::new()
        } else {
            bounded(&plan.target.name, MAX_NAME_CHARS)
        };
        Self {
            mixin: bounded(&plan.callback.owner, MAX_NAME_CHARS),
            target: bounded(&plan.target.owner, MAX_NAME_CHARS),
            method,
            injector: bounded(&plan.at, MAX_NAME_CHARS),
            reason: plan.reason.label().to_owned(),
            detail: plan.reason.detail().map(|d| bounded(d, MAX_NAME_CHARS)),
        }
    }
}

/// Whether a skipped plan is one a real loader would have applied: a family the bounded weave does not run,
/// or a target that is a game class. These are the plans that make a jar `needs-loader`; a skip inside the
/// jar's own classes says nothing about the loader.
pub(crate) fn needs_loader(plan: &SkippedMixinPlan) -> bool {
    plan.reason == MixinSkipReason::UnsupportedFamily
        || plan.reason == MixinSkipReason::TargetOutsideJar
        || plan.target.owner.starts_with("net/minecraft/")
}

/// The plan rows behind a jar's `skipped` count, and that count split by reason label.
///
/// Loader-relevant plans ([`needs_loader`]) come first, each group in the weave's order, so the bounded list
/// always starts with what explains a `needs-loader` label. The counts cover every plan, not just those listed.
pub(crate) fn plan_rows(
    skipped: &[SkippedMixinPlan],
) -> (Vec<RuntimePlan>, BTreeMap<String, usize>) {
    let mut by_reason = BTreeMap::new();
    for plan in skipped {
        *by_reason.entry(plan.reason.label().to_owned()).or_insert(0) += 1;
    }
    let (loader, own): (Vec<&SkippedMixinPlan>, Vec<&SkippedMixinPlan>) =
        skipped.iter().partition(|plan| needs_loader(plan));
    let rows = loader
        .into_iter()
        .chain(own)
        .take(MAX_PLANS_PER_JAR)
        .map(RuntimePlan::from_skipped)
        .collect();
    (rows, by_reason)
}

/// A refusal cause as a row carries it: one line (control characters become spaces), whole up to
/// [`MAX_CAUSE_CHARS`].
pub(crate) fn bounded_cause(cause: &str) -> String {
    bounded(cause, MAX_CAUSE_CHARS)
}

fn bounded(text: &str, limit: usize) -> String {
    let mut out: String = text
        .chars()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.chars().count() > limit {
        out.pop();
        out.push('…');
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeJar {
    pub(crate) file: String,
    pub(crate) loader: String,
    pub(crate) engine: String,
    pub(crate) woven: usize,
    pub(crate) skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) first_skip_reason: Option<String>,
    pub(crate) registrations: usize,
    pub(crate) opaque: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) isolation_failure: Option<String>,
    /// Why the runner refused the jar, whole ([`bounded_cause`]); only on an `engine = "refused"` row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refusal: Option<String>,
    /// The jar's sha256, the key of its `java-content-cache/<sha256>-<role>.json` entry, when the cache looked
    /// it up. The launcher's "clear" action removes those entries by this name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) sha256: Option<String>,
    /// The first skipped plans ([`plan_rows`]); `skipped` is the whole count.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) plans: Vec<RuntimePlan>,
    /// Every skipped plan counted by its reason label.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) skipped_by_reason: BTreeMap<String, usize>,
}

impl RuntimeJar {
    /// The row of a jar the runner refused: nothing ran, so every count is zero.
    pub(crate) fn refused(file: String, cause: &str) -> Self {
        Self {
            file,
            loader: "unknown".to_owned(),
            engine: "refused".to_owned(),
            woven: 0,
            skipped: 0,
            first_skip_reason: None,
            registrations: 0,
            opaque: 0,
            isolation_failure: None,
            refusal: Some(bounded_cause(cause)),
            sha256: None,
            plans: Vec::new(),
            skipped_by_reason: BTreeMap::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeReport {
    pub(crate) schema_version: u32,
    pub(crate) jars: Vec<RuntimeJar>,
    /// Rows the batch had, before the file's bounds dropped any.
    #[serde(default)]
    pub(crate) jars_total: usize,
    /// Refused rows the batch had, before the bounds.
    #[serde(default)]
    pub(crate) refused: usize,
    /// The last opt-in FML preflight of this process; absent when none ran. [`write`] fills it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) fml: Option<RuntimeFml>,
    /// What each jar's game-event listeners did on this process's servers: jar file → event name
    /// (`use_block`, `server_tick_end`...) → counts. [`write`] fills it, and a server's stop rewrites it
    /// ([`refresh_listeners`]) with the session's counts.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) listeners: BTreeMap<String, BTreeMap<String, RuntimeListeners>>,
}

/// One jar's listeners of one game event: how many it kept, how many times they ran (`dispatched`), how
/// many failed and were disabled at that failure (`refused`), and how many are disabled now.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeListeners {
    pub(crate) kept: usize,
    pub(crate) dispatched: u64,
    pub(crate) refused: u64,
    pub(crate) disabled: usize,
}

/// Mod ids the `fml` section lists, sorted and deduplicated.
const MAX_FML_IDS: usize = 64;

/// The real FancyModLoader preflight as the file reports it: `ok` or `failed`, the role and jar count it ran
/// for, FML's own mod count and ids, why it failed and whether `HORIZON_NEOFORGE_STRICT` made that fatal.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RuntimeFml {
    pub(crate) status: String,
    pub(crate) role: String,
    pub(crate) jars: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) mods: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<String>,
    #[serde(default)]
    pub(crate) strict: bool,
}

impl RuntimeFml {
    pub(crate) fn from_report(report: &super::java_mods::fml::FmlReport, strict: bool) -> Self {
        let mut ids: Vec<String> = report
            .mod_ids
            .iter()
            .map(|id| bounded(id, MAX_NAME_CHARS))
            .collect();
        ids.sort();
        ids.dedup();
        ids.truncate(MAX_FML_IDS);
        Self {
            status: if report.succeeded() { "ok" } else { "failed" }.to_owned(),
            role: super::java_mods::fml::role_name(report.role).to_owned(),
            jars: report.jars,
            mods: report.mods,
            ids,
            error: report.failure.as_deref().map(bounded_cause),
            strict,
        }
    }
}

impl RuntimeReport {
    /// A report of `jars`, with its whole counts taken before any bound applies.
    pub(crate) fn new(jars: Vec<RuntimeJar>) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            jars_total: jars.len(),
            refused: jars.iter().filter(|jar| jar.engine == "refused").count(),
            jars,
            fml: None,
            listeners: BTreeMap::new(),
        }
    }
}

/// The report as the file holds it: within [`MAX_JARS`] rows, [`MAX_PLANS_PER_JAR`] plans per row and
/// [`MAX_BYTES`] in all. Plan lists halve until the file fits; rows are dropped from the end only once every
/// list is empty.
pub(crate) fn encode(report: RuntimeReport) -> Vec<u8> {
    let mut report = report;
    report.jars.truncate(MAX_JARS);
    for jar in &mut report.jars {
        jar.plans.truncate(MAX_PLANS_PER_JAR);
        jar.refusal = jar.refusal.as_deref().map(bounded_cause);
    }
    let mut plans = MAX_PLANS_PER_JAR;
    loop {
        let bytes = serde_json::to_vec(&report).expect("runtime report is serializable");
        if bytes.len() <= MAX_BYTES || report.jars.is_empty() {
            return bytes;
        }
        if plans > 0 {
            plans /= 2;
            for jar in &mut report.jars {
                jar.plans.truncate(plans);
            }
        } else {
            let keep = report.jars.len() / 2;
            report.jars.truncate(keep);
        }
    }
}

/// Writes the report under `home`, with the process's last FML preflight attached when one ran and the
/// caller did not set it.
pub(crate) fn write(home: &Path, report: RuntimeReport) -> std::io::Result<()> {
    let mut report = report;
    if report.fml.is_none() {
        report.fml = super::java_mods::fml::last_report()
            .map(|last| RuntimeFml::from_report(&last, super::java_mods::fml::strict_requested()));
    }
    if report.listeners.is_empty() {
        report.listeners = super::java_events::listener_counts();
    }
    let bytes = encode(report);
    fs::create_dir_all(home)?;
    let part = home.join(".java-runtime.json.part");
    fs::write(&part, bytes)?;
    fs::rename(part, home.join("java-runtime.json"))
}

/// Rewrites `home`'s file with the listener counts now, keeping its rows: a server's stop calls it, so the
/// file says what the session's listeners did. Nothing happens without a file to update.
pub(crate) fn refresh_listeners(home: &Path) -> std::io::Result<()> {
    let Ok(bytes) = fs::read(home.join("java-runtime.json")) else {
        return Ok(());
    };
    let Ok(mut report) = serde_json::from_slice::<RuntimeReport>(&bytes) else {
        return Ok(());
    };
    report.listeners = super::java_events::listener_counts();
    write(home, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use horizon_native_java::MethodRef;

    fn method(owner: &str, name: &str) -> MethodRef {
        MethodRef {
            owner: owner.to_owned(),
            name: name.to_owned(),
            descriptor: "()V".to_owned(),
        }
    }

    fn plan(target: &str, reason: MixinSkipReason, at: &str) -> SkippedMixinPlan {
        SkippedMixinPlan {
            target: method(target, "tick"),
            callback: method("demo/mixin/DemoMixin", "onTick"),
            at: at.to_owned(),
            reason,
        }
    }

    fn started(file: &str, skipped: &[SkippedMixinPlan]) -> RuntimeJar {
        let (plans, skipped_by_reason) = plan_rows(skipped);
        RuntimeJar {
            file: file.to_owned(),
            loader: "fabric".to_owned(),
            engine: "native-qualified".to_owned(),
            woven: 1,
            skipped: skipped.len(),
            first_skip_reason: None,
            registrations: 2,
            opaque: 0,
            isolation_failure: None,
            refusal: None,
            sha256: Some("ab".repeat(32)),
            plans,
            skipped_by_reason,
        }
    }

    /// The loader-relevant plans lead the list whatever order the weave skipped them in, each plan names its
    /// mixin, target, method, injector and reason, and the counts split every plan by its label.
    #[test]
    fn plan_rows_lead_with_what_needs_the_loader() {
        let own = plan("demo/Target", MixinSkipReason::Cancellable, "TAIL");
        let game = plan(
            "net/minecraft/world/level/Level",
            MixinSkipReason::TargetOutsideJar,
            "HEAD",
        );
        let weaver = plan(
            "demo/Target",
            MixinSkipReason::Weaver("stack\nshape".into()),
            "RETURN",
        );
        let (rows, counts) = plan_rows(&[own, weaver, game]);
        assert_eq!(
            rows[0],
            RuntimePlan {
                mixin: "demo/mixin/DemoMixin".into(),
                target: "net/minecraft/world/level/Level".into(),
                method: "tick".into(),
                injector: "HEAD".into(),
                reason: "target-outside-jar".into(),
                detail: None,
            }
        );
        assert_eq!(rows[1].reason, "cancellable");
        // A weaver's detail is one line.
        assert_eq!(rows[2].detail.as_deref(), Some("stack shape"));
        assert_eq!(
            counts,
            BTreeMap::from([
                ("cancellable".to_owned(), 1),
                ("target-outside-jar".to_owned(), 1),
                ("weaver-rejected".to_owned(), 1),
            ])
        );
    }

    /// More plans than the bound: the list stops at it, the count by reason does not.
    #[test]
    fn plan_rows_are_bounded_but_counted_whole() {
        let many: Vec<_> = (0..MAX_PLANS_PER_JAR + 9)
            .map(|_| plan("net/minecraft/A", MixinSkipReason::TargetOutsideJar, "HEAD"))
            .collect();
        let (rows, counts) = plan_rows(&many);
        assert_eq!(rows.len(), MAX_PLANS_PER_JAR);
        assert_eq!(counts["target-outside-jar"], MAX_PLANS_PER_JAR + 9);
    }

    /// A refusal keeps every character up to the cause bound: the old 160-character clip is gone.
    #[test]
    fn a_refusal_cause_is_kept_whole() {
        let cause = format!(
            "uncaught exception java/lang/NullPointerException: Tried to register a listener for a null phase \
             (at {}/Listener#register)",
            "com/example/deeply/nested/package".repeat(4)
        );
        assert!(cause.chars().count() > 160);
        let row = RuntimeJar::refused("collective.jar".into(), &format!("{cause}\nnext"));
        assert_eq!(
            row.refusal.as_deref(),
            Some(format!("{cause} next").as_str())
        );
        assert_eq!(row.engine, "refused");

        let hostile = "x".repeat(MAX_CAUSE_CHARS * 3);
        let bounded = bounded_cause(&hostile);
        assert_eq!(bounded.chars().count(), MAX_CAUSE_CHARS);
        assert!(bounded.ends_with('…'));
    }

    /// What the writer encodes reads back as the same report, refused rows and plan lists included.
    #[test]
    fn the_report_round_trips() {
        let report = RuntimeReport::new(vec![
            started(
                "a.jar",
                &[plan(
                    "net/minecraft/B",
                    MixinSkipReason::TargetOutsideJar,
                    "INVOKE",
                )],
            ),
            RuntimeJar::refused(
                "b.jar".into(),
                "Metadata(\"malformed java/lang/Boolean marker\")",
            ),
        ]);
        assert_eq!((report.jars_total, report.refused), (2, 1));
        let back: RuntimeReport = serde_json::from_slice(&encode(report.clone())).unwrap();
        assert_eq!(back, report);
    }

    /// The `fml` section says what the preflight found, bounded like `server-status`'s, and a file without it
    /// (no preflight, or an older game) reads with none.
    #[test]
    fn the_fml_section_reports_the_preflight_and_is_optional() {
        use super::super::execution_role::ExecutionRole;
        let failed = super::super::java_mods::fml::FmlReport {
            role: ExecutionRole::DedicatedServer,
            jars: 2,
            mods: None,
            mod_ids: vec!["neoforge".into(), "minecraft".into(), "neoforge".into()],
            failure: Some(format!(
                "FML probe failed: {}\nnext",
                "x".repeat(MAX_CAUSE_CHARS)
            )),
        };
        let fml = RuntimeFml::from_report(&failed, true);
        assert_eq!(
            (fml.status.as_str(), fml.role.as_str(), fml.jars, fml.strict),
            ("failed", "dedicated-server", 2, true)
        );
        assert_eq!(fml.ids, ["minecraft", "neoforge"]);
        let error = fml.error.clone().unwrap();
        assert_eq!(error.chars().count(), MAX_CAUSE_CHARS);
        assert!(!error.contains('\n'));

        let mut report = RuntimeReport::new(Vec::new());
        report.fml = Some(fml);
        let back: RuntimeReport = serde_json::from_slice(&encode(report.clone())).unwrap();
        assert_eq!(back, report);
        let none: RuntimeReport =
            serde_json::from_slice(&encode(RuntimeReport::new(Vec::new()))).unwrap();
        assert_eq!(none.fml, None);
    }

    /// A schema-1 file written before the plan list and refusal rows still reads, with the new fields empty.
    #[test]
    fn a_file_without_the_new_fields_still_reads() {
        let old = r#"{"schema_version":1,"jars":[{"file":"a.jar","loader":"fabric","engine":"native",
            "woven":0,"skipped":0,"registrations":1,"opaque":0}]}"#;
        let report: RuntimeReport = serde_json::from_str(old).unwrap();
        assert_eq!(report.jars[0].refusal, None);
        assert!(report.jars[0].plans.is_empty());
        assert_eq!(report.jars_total, 0);
    }

    /// A pack whose full plan lists would not fit keeps every row and every count; only the lists shrink.
    #[test]
    fn a_large_pack_shrinks_plan_lists_before_dropping_rows() {
        let skipped: Vec<_> = (0..MAX_PLANS_PER_JAR)
            .map(|i| {
                plan(
                    &format!("net/minecraft/world/level/block/entity/SomeLongBlockEntityName{i}"),
                    MixinSkipReason::TargetOutsideJar,
                    "INVOKE",
                )
            })
            .collect();
        let jars: Vec<RuntimeJar> = (0..MAX_JARS)
            .map(|i| started(&format!("jar-{i:03}.jar"), &skipped))
            .collect();
        let report = RuntimeReport::new(jars);
        let bytes = encode(report);
        assert!(bytes.len() <= MAX_BYTES, "{} bytes", bytes.len());
        let back: RuntimeReport = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.jars.len(), MAX_JARS);
        assert_eq!(back.jars_total, MAX_JARS);
        assert!(back.jars[0].plans.len() < MAX_PLANS_PER_JAR);
        assert_eq!(back.jars[0].skipped, MAX_PLANS_PER_JAR);
        assert_eq!(
            back.jars[0].skipped_by_reason["target-outside-jar"],
            MAX_PLANS_PER_JAR
        );
    }
}

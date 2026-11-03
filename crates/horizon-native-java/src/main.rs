use horizon_native_java::{MethodRef, MixinInjectionPlan, NativeJar, StdHost};
use std::{path::PathBuf, process::ExitCode};

/// Command-line shape: `--json` and `--mixin-plans` may appear anywhere, the
/// remaining arguments are the jar, an optional loader kind, and an optional
/// explicit class.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    json: bool,
    mixin_plans: bool,
    path: String,
    kind: Option<String>,
    class: Option<String>,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Option<Self> {
        let mut json = false;
        let mut mixin_plans = false;
        let mut positional = Vec::new();
        for arg in args {
            if arg == "--json" {
                json = true;
            } else if arg == "--mixin-plans" {
                mixin_plans = true;
            } else {
                positional.push(arg);
            }
        }
        let mut positional = positional.into_iter();
        Some(Self {
            json,
            mixin_plans,
            path: positional.next()?,
            kind: positional.next(),
            class: positional.next(),
        })
    }
}

/// Render a method as `owner#name(descriptor)`.
///
/// The descriptor keeps its own parentheses, so a concrete method reads
/// `demo/Target#greet(Ljava/lang/String;)Ljava/lang/String;` and a
/// descriptor-less `@Inject(method = "greet")` reads `demo/Target#greet`.
fn method_label(method: &MethodRef) -> String {
    format!("{}#{}{}", method.owner, method.name, method.descriptor)
}

/// Sorted, deduplicated `owner#name(descriptor)` labels for target methods.
fn mixin_target_labels<'a>(targets: impl IntoIterator<Item = &'a MethodRef>) -> Vec<String> {
    let mut labels = targets.into_iter().map(method_label).collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    labels
}

/// Sort key for the plan report, mirroring the discovery order.
fn mixin_plan_sort_key(
    plan: &MixinInjectionPlan,
) -> (&str, &str, &str, &str, &str, &str, &str, bool) {
    (
        &plan.target.owner,
        &plan.target.name,
        &plan.target.descriptor,
        &plan.at,
        &plan.callback.owner,
        &plan.callback.name,
        &plan.callback.descriptor,
        plan.cancellable,
    )
}

/// Report rows for discovered plans, sorted and deduplicated.
///
/// `callback` carries the mixin class and method (`owner#name`) because a
/// callback only makes sense together with the class that declares it;
/// `callback_descriptor` holds the rest of its signature.
fn mixin_plan_entries(plans: &[MixinInjectionPlan]) -> Vec<serde_json::Value> {
    let mut plans = plans.to_vec();
    plans.sort_by(|a, b| mixin_plan_sort_key(a).cmp(&mixin_plan_sort_key(b)));
    plans.dedup();
    plans
        .iter()
        .map(|plan| {
            serde_json::json!({
                "target": plan.target.owner,
                "method": plan.target.name,
                "descriptor": plan.target.descriptor,
                "callback": format!("{}#{}", plan.callback.owner, plan.callback.name),
                "callback_descriptor": plan.callback.descriptor,
                "at": plan.at,
                "cancellable": plan.cancellable,
            })
        })
        .collect()
}

/// The `--mixin-plans` report: planning only, no entrypoint is required.
fn mixin_plans_json(jar: &NativeJar) -> serde_json::Value {
    let plans = jar.mixin_injection_plans();
    serde_json::json!({
        "status": "ok",
        "targets": mixin_target_labels(plans.iter().map(|plan| &plan.target)),
        "plans": mixin_plan_entries(&plans),
    })
}

fn main() -> ExitCode {
    let Some(cli) = Cli::parse(std::env::args().skip(1)) else {
        eprintln!(
            "usage: horizon-native-java [--json] <mod.jar> [main|client|server|neoforge] [class]"
        );
        return ExitCode::FAILURE;
    };
    let Cli {
        json,
        mixin_plans,
        path,
        kind: requested_kind,
        class: explicit_class,
    } = cli;
    let mut kind = requested_kind.clone().unwrap_or_else(|| "main".to_owned());
    let jar = match NativeJar::from_path(&PathBuf::from(path)) {
        Ok(jar) => jar,
        Err(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status": "error", "stage": "load", "error": error.to_string()})
                );
            } else {
                eprintln!("error loading jar: {error}");
            }
            return ExitCode::FAILURE;
        }
    };
    // Planning is a separate question from execution: report what the jar's
    // annotations would weave and stop, even when the jar declares no
    // entrypoint at all.
    if mixin_plans {
        println!("{}", mixin_plans_json(&jar));
        return ExitCode::SUCCESS;
    }
    let entrypoints = if let Some(class) = explicit_class {
        vec![horizon_native_java::Entrypoint {
            class,
            method: None,
            adapter: None,
        }]
    } else {
        let selected_kind = match requested_kind.as_deref() {
            Some(_) => Some(kind.clone()),
            None => ["main", "client", "server", "neoforge"]
                .into_iter()
                .find(|candidate| !jar.discover_entrypoints(candidate).is_empty())
                .map(str::to_owned),
        };
        if let Some(selected_kind) = selected_kind {
            kind = selected_kind.clone();
            jar.discover_entrypoints(&selected_kind)
        } else {
            Vec::new()
        }
    };
    if entrypoints.is_empty() {
        if json {
            println!(
                "{}",
                serde_json::json!({"status": "error", "stage": "discover", "kind": kind, "error": "no entrypoint"})
            );
        } else {
            eprintln!("no {kind} entrypoint in metadata; pass its class as the third argument");
        }
        return ExitCode::FAILURE;
    };
    let mut host = jar.seed_host(StdHost::new());
    let mut calls = 0;
    let mut error = None;
    for entry in &entrypoints {
        if let Some(adapter) = entry.adapter.as_deref() {
            error = Some(format!(
                "{}: Fabric entrypoint adapter {adapter} is not supported by the native path",
                entry.class
            ));
            break;
        }
        match jar.execute(&entry.class, entry.method.as_deref(), &kind, &mut host) {
            Ok(report) => calls += report.calls,
            Err(entry_error) => {
                error = Some(format!("{}: {entry_error}", entry.class));
                break;
            }
        }
    }
    match error {
        None => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({
                        "status": "ok",
                        "kind": kind,
                        "entrypoints": entrypoints.iter().map(|entry| &entry.class).collect::<Vec<_>>(),
                        "host_calls": calls,
                        "payload_directions": host
                            .payload_registrations()
                            .iter()
                            .map(|registration| registration.direction.clone())
                            .collect::<Vec<_>>(),
                        "registrations": host
                            .registrations()
                            .iter()
                            .map(|entry| serde_json::json!({
                                "registry": entry.registry,
                                "id": entry.id,
                                "typed": entry.declaration.is_some(),
                            }))
                            .collect::<Vec<_>>(),
                        "listeners": host.listener_registrations().count(),
                        "events": host.posted_events().len(),
                        "mixin_targets": mixin_target_labels(
                            jar.discovered_mixin_targets().iter()
                        ),
                        "mixin_plans": jar.mixin_injection_plans().len(),
                    })
                );
            } else {
                println!(
                    "native {} entrypoint(s) ran ({} host calls)",
                    entrypoints.len(),
                    calls
                );
            }
            ExitCode::SUCCESS
        }
        Some(error) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"status": "error", "stage": "execute", "error": error.to_string()})
                );
            } else {
                eprintln!("native execution stopped: {error}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;

    fn parse(args: &[&str]) -> Option<Cli> {
        Cli::parse(args.iter().map(|arg| (*arg).to_owned()))
    }

    #[test]
    fn json_flag_is_position_independent() {
        let expected = Cli {
            json: true,
            mixin_plans: false,
            path: "mod.jar".into(),
            kind: Some("client".into()),
            class: Some("demo.Main".into()),
        };
        assert_eq!(
            parse(&["--json", "mod.jar", "client", "demo.Main"]),
            Some(expected.clone())
        );
        assert_eq!(
            parse(&["mod.jar", "--json", "client", "demo.Main"]),
            Some(expected)
        );
    }

    #[test]
    fn mixin_plan_flag_is_position_independent() {
        let expected = Cli {
            json: false,
            mixin_plans: true,
            path: "mod.jar".into(),
            kind: None,
            class: None,
        };
        assert_eq!(parse(&["--mixin-plans", "mod.jar"]), Some(expected.clone()));
        assert_eq!(parse(&["mod.jar", "--mixin-plans"]), Some(expected));
    }

    #[test]
    fn bare_jar_is_a_valid_invocation() {
        assert_eq!(
            parse(&["mod.jar"]),
            Some(Cli {
                json: false,
                mixin_plans: false,
                path: "mod.jar".into(),
                kind: None,
                class: None,
            })
        );
        assert_eq!(parse(&[]), None);
    }
}

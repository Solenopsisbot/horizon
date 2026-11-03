use horizon_native_java::{NativeJar, StdHost};
use std::{path::PathBuf, process::ExitCode};

/// Command-line shape: `--json` may appear anywhere, the remaining arguments
/// are the jar, an optional loader kind, and an optional explicit class.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cli {
    json: bool,
    path: String,
    kind: Option<String>,
    class: Option<String>,
}

impl Cli {
    fn parse(args: impl IntoIterator<Item = String>) -> Option<Self> {
        let mut json = false;
        let mut positional = Vec::new();
        for arg in args {
            if arg == "--json" {
                json = true;
            } else {
                positional.push(arg);
            }
        }
        let mut positional = positional.into_iter();
        Some(Self {
            json,
            path: positional.next()?,
            kind: positional.next(),
            class: positional.next(),
        })
    }
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
            path: "mod.jar".into(),
            kind: Some("client".into()),
            class: Some("demo.Main".into()),
        };
        assert_eq!(parse(&["--json", "mod.jar", "client", "demo.Main"]), Some(expected.clone()));
        assert_eq!(parse(&["mod.jar", "--json", "client", "demo.Main"]), Some(expected));
    }

    #[test]
    fn bare_jar_is_a_valid_invocation() {
        assert_eq!(
            parse(&["mod.jar"]),
            Some(Cli {
                json: false,
                path: "mod.jar".into(),
                kind: None,
                class: None,
            })
        );
        assert_eq!(parse(&[]), None);
    }
}

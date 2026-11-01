use horizon_native_java::{NativeJar, StdHost};
use std::{path::PathBuf, process::ExitCode};

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let json = args.next().is_some_and(|arg| arg == "--json");
    let Some(path) = args.next() else {
        eprintln!(
            "usage: horizon-native-java [--json] <mod.jar> [main|client|server|neoforge] [class]"
        );
        return ExitCode::FAILURE;
    };
    let requested_kind = args.next();
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
    let entrypoints = if let Some(class) = args.next() {
        vec![horizon_native_java::Entrypoint {
            class,
            method: None,
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
    let mut host = StdHost::new();
    let mut calls = 0;
    let mut error = None;
    for entry in &entrypoints {
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

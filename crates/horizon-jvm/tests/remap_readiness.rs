//! Gated checks for the local intermediary mappings and client install probe.

use std::{fs, path::PathBuf, process::Command};

fn mapping_path() -> Option<PathBuf> {
    let value = std::env::var_os("HORIZON_MAPPINGS")?;
    std::env::split_paths(&value).find(|path| path.is_file())
}

#[test]
fn real_mappings_report_a_built_tree_when_installed() {
    let Some(path) = mapping_path() else {
        eprintln!("skipping: HORIZON_MAPPINGS does not name a real mappings file");
        return;
    };
    let output = Command::new(env!("CARGO_BIN_EXE_horizon-jvm"))
        .arg("--remap-report")
        .arg("--mappings")
        .arg(&path)
        .output()
        .expect("probe should start");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "probe failed: {text}");
    assert!(
        text.lines().any(|line| line.starts_with("remap: ok ")),
        "{text}"
    );
}

#[test]
fn client_readiness_is_bounded_without_starting_a_game() {
    let root = tempfile_root();
    let output = Command::new(env!("CARGO_BIN_EXE_horizon-jvm"))
        .arg("--entry-kind")
        .arg("client")
        .arg("--client-home")
        .arg(&root)
        .arg("--provider-report")
        .arg("--json")
        .output()
        .expect("probe should start");
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "probe failed: {text}");
    assert!(text.contains("client-launch: needs assets ("), "{text}");
    let _ = fs::remove_dir_all(root);
}

fn tempfile_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "horizon-jvm-readiness-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    fs::create_dir_all(&root).expect("temporary readiness root");
    root
}

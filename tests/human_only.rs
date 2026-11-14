//! The commands that decide for the human refuse to run without a terminal,
//! so an agent's Bash (which has none) cannot run them. Books: REQ-1, V-1.

use std::process::{Command, Stdio};

fn hookwatch(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_hookwatch"))
        .args(args)
        .env("HOOKWATCH_HOME", home)
        .stdin(Stdio::null()) // not a terminal, as under an agent
        .output()
        .unwrap()
}

#[test]
fn human_only_commands_refuse_without_a_terminal_and_write_nothing() {
    let home = std::env::temp_dir().join(format!("hookwatch-human-only-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let jigs = home.join("candidate.json");
    std::fs::write(&jigs, r#"{"jigs": []}"#).unwrap();
    let jigs = jigs.to_str().unwrap();
    for args in [
        vec!["accept", "x"],
        vec!["reject", "x", "--why", "y"],
        vec!["override", "x", "--right"],
        vec!["put-jigs", jigs],
        vec!["prune"],
    ] {
        let out = hookwatch(&home, &args);
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{args:?} ran without a terminal");
        assert!(err.contains("needs a terminal"), "{args:?}: {err}");
    }
    // Refused before anything was opened or written.
    assert!(!home.join("ledger.db").exists());
    assert!(!home.join("jigs.json").exists());
    std::fs::remove_dir_all(&home).unwrap();
}

#[test]
fn reading_commands_run_without_a_terminal() {
    let home = std::env::temp_dir().join(format!("hookwatch-readers-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    for args in [
        vec!["status"],
        vec!["jigs"],
        vec!["proposals"],
        vec!["panel"],
    ] {
        let out = hookwatch(&home, &args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    std::fs::remove_dir_all(&home).unwrap();
}

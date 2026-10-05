//! Log lines on stderr are logfmt by default and JSON with
//! `KRAB_LOG_FORMAT=json` (CLI-61).
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::Command;

fn log_lines(format: Option<&str>) -> Vec<String> {
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_krab"));
    cmd.current_dir(&fixtures)
        .args(["--no-daemon", "inventory", "targets"])
        .env("RUST_LOG", "info")
        .env_remove("KRAB_LOG_FORMAT");
    if let Some(f) = format {
        cmd.env("KRAB_LOG_FORMAT", f);
    }
    let out = cmd.output().unwrap();
    String::from_utf8_lossy(&out.stderr)
        .lines()
        .map(str::to_string)
        .collect()
}

fn python3() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("pass")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[test]
fn logs_are_logfmt_by_default_and_json_on_request() {
    if !python3() {
        eprintln!("python3 not available; skipping");
        return;
    }
    let logfmt = log_lines(None);
    let line = logfmt
        .iter()
        .find(|l| l.contains("Python resolvers configured"))
        .unwrap_or_else(|| panic!("no log line in {logfmt:?}"));
    assert!(line.starts_with("ts="), "{line}");
    assert!(
        line.contains(" level=info target=krab::app message=\"Python resolvers configured\""),
        "{line}"
    );

    let json = log_lines(Some("json"));
    let line = json
        .iter()
        .find(|l| l.contains("Python resolvers configured"))
        .unwrap_or_else(|| panic!("no log line in {json:?}"));
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["level"], "INFO", "{line}");
    assert_eq!(v["target"], "krab::app", "{line}");
    assert_eq!(v["message"], "Python resolvers configured", "{line}");
}

#[test]
fn an_unknown_format_falls_back_to_logfmt_with_a_warning() {
    let lines = log_lines(Some("yaml"));
    assert!(
        lines.iter().any(|l| l.starts_with("ts=")
            && l.contains("level=warn")
            && l.contains("unknown KRAB_LOG_FORMAT")
            && l.contains("value=yaml")),
        "{lines:?}"
    );
}

#[test]
fn an_empty_format_is_the_default_without_a_warning() {
    let lines = log_lines(Some(""));
    assert!(
        !lines.iter().any(|l| l.contains("unknown KRAB_LOG_FORMAT")),
        "{lines:?}"
    );
}

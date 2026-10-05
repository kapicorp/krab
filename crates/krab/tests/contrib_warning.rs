//! A krab-only (`contrib`) resolver warns once per name and render unless
//! `.kapitan` opts in with `inventory.contrib-resolvers: true`; the output
//! stays the same either way.
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::Command;

const TARGET: &str =
    "parameters:\n  a: ${sha256:x,8}\n  b: ${sha256:y,8}\n  c: ${truncate:abcdefghij,8}\n";
const BACKEND: &str = "global:\n  inventory-backend: omegaconf\n";

/// `--json` prints diagnostics as JSON lines on stdout before the document:
/// returns (document, diagnostics).
fn render(dir: &Path) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "--json", "inventory", "-t", "t"])
        .current_dir(dir)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(out.status.success(), "{stdout}");
    let (diags, doc): (Vec<&str>, Vec<&str>) =
        stdout.lines().partition(|l| l.starts_with("{\"severity\""));
    (doc.join("\n"), diags.join("\n"))
}

fn warning(name: &str) -> String {
    format!(
        "\"message\":\"krab extension: `{name}` is not a kapitan resolver; kapitan 0.36.3 fails \
         with Unsupported interpolation type. Set inventory.contrib-resolvers: true to silence\""
    )
}

#[test]
fn contrib_resolvers_warn_unless_enabled() {
    let dir = std::env::temp_dir().join(format!("krab-contrib-warning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(dir.join("inventory/targets/t.yml"), TARGET).unwrap();

    std::fs::write(dir.join(".kapitan"), BACKEND).unwrap();
    let (warned_out, warned) = render(&dir);
    assert_eq!(warned.matches(&warning("sha256")).count(), 1, "{warned}");
    assert_eq!(warned.matches(&warning("truncate")).count(), 1, "{warned}");

    std::fs::write(
        dir.join(".kapitan"),
        format!("{BACKEND}inventory:\n  contrib-resolvers: true\n"),
    )
    .unwrap();
    let (quiet_out, quiet) = render(&dir);
    assert!(!quiet.contains("krab extension"), "{quiet}");
    assert_eq!(warned_out, quiet_out);

    // A same-named resolver from resolvers.py is kapitan's, not krab's.
    let has_python = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if has_python {
        std::fs::write(dir.join(".kapitan"), BACKEND).unwrap();
        std::fs::write(
            dir.join("inventory/resolvers.py"),
            "def sha256(v, n):\n    return 'py'\ndef pass_resolvers():\n    return {'sha256': sha256}\n",
        )
        .unwrap();
        let (_, stderr) = render(&dir);
        assert!(!stderr.contains(&warning("sha256")), "{stderr}");
        assert_eq!(stderr.matches(&warning("truncate")).count(), 1, "{stderr}");
    } else {
        eprintln!("python3 not available; skipping the resolvers.py case");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

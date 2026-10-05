//! A `resolvers.py` that cannot be imported is reported once, with the real
//! exception; targets render with the built-in resolvers, and only a target
//! calling one of the file's names fails.
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::{Command, Output};

fn krab(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_krab"))
        .arg("--no-daemon")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn a_broken_resolvers_py_leaves_the_builtins_working() {
    if !Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("python3 not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-broken-resolvers-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("inventory/targets/plain.yml"),
        "parameters:\n  a: ${oc.select:nope,x}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("inventory/targets/uses.yml"),
        "parameters:\n  b: ${shout:x}\n",
    )
    .unwrap();
    for (body, exception) in [
        ("def broken(:\n", "SyntaxError"),
        (
            "import not_a_module_xyz\ndef pass_resolvers():\n    return {}\n",
            "No module named 'not_a_module_xyz'",
        ),
        (
            "def shout(s):\n    return s.upper()\n",
            "must define a function pass_resolvers()",
        ),
    ] {
        std::fs::write(dir.join("inventory/resolvers.py"), body).unwrap();

        let out = krab(&dir, &["--json", "inventory", "check"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let diags: Vec<serde_json::Value> = stdout
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let import: Vec<_> = diags
            .iter()
            .filter(|d| d["code"] == "inventory::python_resolvers")
            .collect();
        assert_eq!(import.len(), 1, "{body}: {stdout}");
        assert!(
            import[0]["message"].as_str().unwrap().contains(exception),
            "{body}: {stdout}"
        );
        let unknown: Vec<_> = diags
            .iter()
            .filter(|d| d["code"] == "interpolation::unknown_resolver")
            .collect();
        assert_eq!(unknown.len(), 1, "{body}: {stdout}");
        assert_eq!(unknown[0]["target"], "uses");
        assert!(
            unknown[0]["help"]
                .as_str()
                .unwrap()
                .contains("failed to import"),
            "{body}: {stdout}"
        );
        assert!(stderr.contains("1 targets rendered, 1 failed"), "{stderr}");

        let out = krab(&dir, &["inventory", "-t", "plain"]);
        assert!(
            out.status.success(),
            "{body}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

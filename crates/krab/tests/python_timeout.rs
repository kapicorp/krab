//! A Python request that runs past `compile.python-timeout` stops the worker
//! and fails with a diagnostic naming the target and the component or
//! resolver, instead of hanging the command. Needs a `python3` (with `kadet`
//! importable for the kadet case), and skips otherwise.
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const DOT_KAPITAN: &str =
    "global:\n  inventory-backend: omegaconf\ncompile:\n  python-timeout: 1\n";

fn python_has(module: &str) -> bool {
    Command::new("python3")
        .args(["-c", &format!("import {module}")])
        .output()
        .is_ok_and(|o| o.status.success())
}

fn repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("krab-timeout-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, content) in [(".kapitan", DOT_KAPITAN)].iter().chain(files) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    dir
}

/// Run krab in `dir` and check it gave up well before the 30 s sleep ends.
fn krab(dir: &Path, args: &[&str]) -> (Output, String) {
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .arg("--no-daemon")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "took {:?}",
        started.elapsed()
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    (out, stderr)
}

#[test]
fn a_kadet_component_past_the_deadline_fails_naming_target_and_component() {
    if !python_has("kadet") {
        eprintln!("python3 with kadet not available; skipping");
        return;
    }
    let dir = repo(
        "kadet",
        &[
            (
                "components/sleepy/__init__.py",
                "import time\n\n\ndef main():\n    time.sleep(30)\n    return {}\n",
            ),
            (
                "inventory/targets/t.yml",
                "parameters:\n  kapitan:\n    compile:\n      - output_path: out\n        input_type: kadet\n        output_type: yaml\n        input_paths:\n          - components/sleepy\n",
            ),
        ],
    );
    let (out, stderr) = krab(&dir, &["compile", "--python", "python3"]);
    assert!(!out.status.success(), "{stderr}");
    for needle in ["t:", "components/sleepy", "python-timeout"] {
        assert!(stderr.contains(needle), "no `{needle}` in: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_resolver_past_the_deadline_fails_naming_target_and_resolver() {
    if !python_has("sys") {
        eprintln!("python3 not available; skipping");
        return;
    }
    let dir = repo(
        "resolver",
        &[
            (
                "inventory/resolvers.py",
                "import time\n\n\ndef slow():\n    time.sleep(30)\n    return 'x'\n\n\ndef pass_resolvers():\n    return {'slow': slow}\n",
            ),
            ("inventory/targets/t.yml", "parameters:\n  r: ${slow:}\n"),
        ],
    );
    let (out, stderr) = krab(&dir, &["inventory", "-t", "t"]);
    assert!(!out.status.success(), "{stderr}");
    for needle in ["[t]", "`slow`", "python-timeout"] {
        assert!(stderr.contains(needle), "no `{needle}` in: {stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

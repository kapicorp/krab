//! A kadet component may write to file descriptor 1 directly (a child
//! process, a C extension, `os.write`). That must not reach the evaluator's
//! reply stream. Needs a `python3` with `kadet` importable, and skips
//! otherwise.
#![allow(clippy::print_stderr)]

use std::process::Command;

const COMPONENT: &str = "import os
import subprocess


def main():
    subprocess.run([\"echo\", \"x\"], check=True)
    os.write(1, b\"x\")
    return {\"out\": {\"a\": 1}}
";

const TARGET: &str = "parameters:
  kapitan:
    compile:
      - output_path: out
        input_type: kadet
        output_type: yaml
        input_paths:
          - components/noisy
";

#[test]
fn a_component_writing_to_fd_1_still_compiles() {
    if !Command::new("python3")
        .args(["-c", "import kadet"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("python3 with kadet not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-kadet-stdout-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, content) in [
        (".kapitan", "global:\n  inventory-backend: omegaconf\n"),
        ("components/noisy/__init__.py", COMPONENT),
        ("inventory/targets/t.yml", TARGET),
    ] {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile", "--python", "python3"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("compiled/t/out/out.yaml")).unwrap(),
        "a: 1\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

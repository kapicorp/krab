//! A kadet item that imports a helper an earlier item of the same target
//! already imported depends on what the helper read at import time (#184).
//! Needs a `python3` with `kadet` importable, and skips otherwise.
#![allow(clippy::print_stderr)]

use std::path::Path;
use std::process::Command;

const ITEM: &str =
    "from components.helper import VAL\n\n\ndef main():\n    return {\"v\": {\"v\": VAL}}\n";

fn target(val: &str) -> String {
    format!(
        "parameters:\n  val: {val}\n  kapitan:\n    compile:\n      - {{input_type: kadet, output_path: a, input_paths: [components/a]}}\n      - {{input_type: kadet, output_path: b, input_paths: [components/b]}}\n"
    )
}

fn compile(dir: &Path) {
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile", "--python", "python3"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn an_item_reusing_an_imported_helper_records_its_reads() {
    if !Command::new("python3")
        .args(["-c", "import kadet"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("python3 with kadet not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-kadet-shared-helper-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (rel, content) in [
        (
            "components/helper.py",
            "from kapitan.inputs.kadet import inventory\n\nVAL = inventory().parameters.val\n",
        ),
        ("components/a/__init__.py", ITEM),
        ("components/b/__init__.py", ITEM),
        ("inventory/targets/t.yml", &target("one")),
    ] {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    compile(&dir);
    std::fs::write(dir.join("inventory/targets/t.yml"), target("two")).unwrap();
    compile(&dir);
    for item in ["a", "b"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("compiled/t/{item}/v.yaml"))).unwrap(),
            "v: two\n",
            "item {item}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

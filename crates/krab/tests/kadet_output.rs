//! A kadet component's multi-line string is written the way kapitan 0.36.3
//! writes it: `compile.yaml-multiline-string-style`, literal by default
//! (`tests/fixtures/kadet-output`, expected output in
//! `tests/fixtures/kadet-output-expected`).

use std::path::{Path, PathBuf};
use std::process::Command;

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Compile the fixture with `dot_kapitan` as its `.kapitan` and return the
/// ConfigMap the component wrote.
fn compile(name: &str, dot_kapitan: &str) -> String {
    let dir = std::env::temp_dir().join(format!("krab-kadet-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&fixtures().join("kadet-output"), &dir);
    std::fs::write(dir.join(".kapitan"), dot_kapitan).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = std::fs::read_to_string(dir.join("compiled/cm/manifests/cm.yaml")).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

fn expected(style: &str) -> String {
    std::fs::read_to_string(
        fixtures()
            .join("kadet-output-expected")
            .join(style)
            .join("cm.yaml"),
    )
    .unwrap()
}

#[test]
fn multiline_strings_are_literal_blocks_by_default() {
    assert_eq!(
        compile("default", "global:\n  inventory-backend: omegaconf\n"),
        expected("literal")
    );
}

#[test]
fn the_compile_setting_wins_over_the_inventory_one() {
    let dot_kapitan = "global:\n  inventory-backend: omegaconf\ninventory:\n  multiline-string-style: literal\ncompile:\n  yaml-multiline-string-style: double-quotes\n";
    assert_eq!(
        compile("double-quotes", dot_kapitan),
        expected("double-quotes")
    );
}

#[test]
fn the_folded_style_keeps_line_breaks() {
    let dot_kapitan = "global:\n  inventory-backend: omegaconf\ncompile:\n  yaml-multiline-string-style: folded\n";
    assert_eq!(compile("folded", dot_kapitan), expected("folded"));
}

/// A kadet file whose value is a string, under a YAML or JSON output type,
/// is still written as one scalar and warns, suggesting `output_type: plain`
/// (#126). A mapping does not warn.
#[test]
fn a_string_file_under_yaml_or_json_warns() {
    let dir = std::env::temp_dir().join(format!("krab-kadet-string-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("components/text")).unwrap();
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("components/text/__init__.py"),
        "from kapitan.inputs.kadet import BaseObj\n\n\ndef main(input_params):\n    obj = BaseObj()\n    obj.root.Dockerfile = \"FROM x\\n\"\n    if input_params.get(\"cm\"):\n        obj.root.cm = {\"a\": 1}\n    return obj\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("inventory/targets/t.yml"),
        "parameters:\n  kapitan:\n    vars:\n      target: t\n    compile:\n      - input_type: kadet\n        input_paths: [components/text]\n        output_path: y\n        input_params: {cm: true}\n      - input_type: kadet\n        input_paths: [components/text]\n        output_path: j\n        output_type: json\n        input_params: {cm: true}\n      - input_type: kadet\n        input_paths: [components/text]\n        output_path: p\n        output_type: plain\n",
    )
    .unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(), "{stderr}");
    let read = |p: &str| std::fs::read_to_string(dir.join("compiled/t").join(p)).unwrap();
    assert_eq!(read("y/Dockerfile.yaml"), "|\n  FROM x\n");
    assert_eq!(read("j/Dockerfile.json"), "\"FROM x\\n\"");
    assert_eq!(read("p/Dockerfile"), "FROM x\n");
    let warnings: Vec<&str> = stderr.lines().filter(|l| l.contains("warning:")).collect();
    assert_eq!(warnings.len(), 2, "{stderr}");
    assert!(
        warnings
            .iter()
            .all(|w| w.contains("Dockerfile") && w.contains("output_type: plain")),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

//! Each target's kadet output depends only on that target: a module the
//! component imports keeps state from the target that first imported it, so
//! no evaluator may serve two targets. Needs a `python3` with `kadet`
//! importable, and skips otherwise.
#![allow(clippy::print_stderr)]

use std::process::Command;

const FILES: &[(&str, &str)] = &[
    (
        "components/shared/__init__.py",
        "from .state import NAME\n\n\ndef main():\n    return {\"name\": {\"name\": NAME}}\n",
    ),
    (
        "components/shared/state.py",
        "from kapitan.inputs.kadet import inventory\n\nNAME = inventory().parameters.name\n",
    ),
];

const TARGET: &str = "parameters:
  name: NAME
  kapitan:
    compile:
      - output_path: out
        input_type: kadet
        output_type: yaml
        input_paths:
          - components/shared
";

/// Report why the test does not run. CI installs what these tests need, so
/// there a skip is a failure rather than a silent pass.
fn skip(why: &str) {
    assert!(
        std::env::var_os("CI").is_none(),
        "{why}, and CI must run this test"
    );
    eprintln!("{why}; skipping");
}

#[test]
fn a_full_compile_gives_each_target_its_own_evaluator() {
    if !Command::new("python3")
        .args(["-c", "import kadet"])
        .output()
        .is_ok_and(|o| o.status.success())
    {
        skip("python3 with kadet not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-kadet-isolation-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut files: Vec<(String, String)> = FILES
        .iter()
        .map(|(p, c)| (p.to_string(), c.to_string()))
        .collect();
    for t in ["a", "b"] {
        files.push((
            format!("inventory/targets/{t}.yml"),
            TARGET.replace("NAME", t),
        ));
    }
    for (rel, content) in &files {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    // One compile thread: a shared evaluator would serve a, then b.
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile", "-p", "1", "--python", "python3"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join("compiled/.krab-manifest.json")).unwrap(),
    )
    .unwrap();
    for t in ["a", "b"] {
        assert_eq!(
            std::fs::read_to_string(dir.join(format!("compiled/{t}/out/name.yaml"))).unwrap(),
            format!("name: {t}\n"),
            "target {t}"
        );
        let deps = &manifest["targets"][t]["deps"];
        assert!(
            deps.as_object()
                .unwrap()
                .contains_key("components/shared/state.py"),
            "target {t} deps: {deps}"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

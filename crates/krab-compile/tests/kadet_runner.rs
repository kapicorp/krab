//! Evaluates the fixture kadet component through the evaluator and its
//! bundled `kapitan` package: needs a `python3` with `kadet` and `jinja2`
//! importable, and skips otherwise.
// A skipped test that says nothing looks like a passing one, so these report
// why they did not run.
#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process::Command;

use krab_compile::inputs::Reads;
use krab_compile::inputs::kadet::KadetPool;
use krab_compile::python::PythonCmd;
use serde_json::{Value, json};

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/fixtures/kadet")
        .canonicalize()
        .unwrap()
}

fn python_has(modules: &str) -> bool {
    Command::new("python3")
        .args(["-c", &format!("import {modules}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn evaluates_a_component_without_the_python_kapitan() {
    if !python_has("kadet, jinja2") {
        eprintln!("python3 with kadet and jinja2 not available; skipping");
        return;
    }
    let root = fixture();
    let temp = std::env::temp_dir().join(format!("krab-kadet-test-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let search_paths = [root.clone(), root.join("lib")];
    let init = json!({
        "cwd": root,
        "inventory_file": root.join("inventory.json"),
        "krab_version": "test",
        "settings": {
            "search_paths": search_paths,
            "reveal": false,
            "embed_refs": true,
            "refs_path": root.join("refs"),
            "indent": 2,
        },
    });
    let pool = KadetPool::new(PythonCmd::parse("python3").unwrap(), init).unwrap();
    let mut reads = Reads::default();
    let output = pool
        .eval(
            "app.web",
            &root.join("components/greeter"),
            &json!({ "flavour": "plain" }),
            &temp.join("compiled"),
            &temp,
            &mut reads,
        )
        .unwrap_or_else(|e| panic!("evaluation failed: {e}"));

    assert_eq!(output["greeting"]["kind"], "Greeting");
    assert_eq!(output["greeting"]["text"], "hello from web");
    assert_eq!(output["greeting"]["empty"], json!({}));
    assert_eq!(
        output["rendered"], "== WEB ==\nab x319x3x1 k: v",
        "{:?}",
        output["rendered"]
    );
    assert_eq!(output["api_replicas"], 1);
    assert_eq!(output["ports"], json!({ "app.web": 80, "app.api": 8080 }));
    assert_eq!(output["target"], "app.web");
    assert_eq!(output["pruned"], json!({ "keep": 1, "nested": {} }));
    assert_eq!(output["targets"], json!(["app.api", "app.web"]));
    assert_eq!(output["params"], "plain");
    let modules: Vec<&str> = output["kapitan_modules"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    for m in [
        "kapitan",
        "kapitan.inputs.kadet",
        "kapitan.utils",
        "kapitan.resources",
    ] {
        assert!(modules.contains(&m), "{modules:?}");
    }
    assert!(
        !modules.contains(&"kapitan.cli") && !modules.contains(&"kapitan.inventory"),
        "an installed kapitan leaked in: {modules:?}"
    );

    // What the compile depends on: the component and its library, the
    // template, the other target, the topic aggregation (every target), and
    // the parts of the own document that were read.
    let files: Vec<String> = reads
        .files
        .iter()
        .map(|p| p.strip_prefix(&root).unwrap().to_string_lossy().to_string())
        .collect();
    for f in [
        "components/greeter/__init__.py",
        "lib/greetlib/__init__.py",
        "templates/banner.j2",
    ] {
        assert!(files.iter().any(|x| x == f), "{f} not in {files:?}");
    }
    assert!(
        reads.globals.contains(&"app.api".to_string()),
        "{:?}",
        reads.globals
    );
    assert!(
        reads.globals.contains(&"*".to_string()),
        "{:?}",
        reads.globals
    );
    for k in [
        "parameters.greeting",
        "parameters.name",
        "parameters.kapitan",
    ] {
        assert!(
            reads.doc_keys.contains(&k.to_string()),
            "{:?}",
            reads.doc_keys
        );
    }
    assert!(
        !reads.doc_keys.contains(&"*".to_string()),
        "key reads should stay precise: {:?}",
        reads.doc_keys
    );
    let _ = std::fs::remove_dir_all(&temp);
}

#[test]
fn undeclared_topic_is_a_compile_error() {
    if !python_has("kadet") {
        eprintln!("python3 with kadet not available; skipping");
        return;
    }
    let root = fixture();
    let temp = std::env::temp_dir().join(format!("krab-kadet-test2-{}", std::process::id()));
    std::fs::create_dir_all(&temp).unwrap();
    let init = json!({
        "cwd": root,
        "inventory_file": root.join("inventory.json"),
        "settings": { "search_paths": [root.clone(), root.join("lib")] },
    });
    let pool = KadetPool::new(PythonCmd::parse("python3").unwrap(), init).unwrap();
    let mut reads = Reads::default();
    // app.api produces the topic but does not declare `consume: true`.
    let err = pool
        .eval(
            "app.api",
            &root.join("components/greeter"),
            &json!({}),
            &temp.join("compiled"),
            &temp,
            &mut reads,
        )
        .expect_err("reading an undeclared topic must fail");
    assert!(
        err.contains("parameters.kapitan.topics.ports.consume: true"),
        "{err}"
    );
    let _ = std::fs::remove_dir_all(&temp);
}

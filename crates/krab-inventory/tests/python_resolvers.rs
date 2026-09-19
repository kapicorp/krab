//! Resolvers from a user `resolvers.py`, run through the Python worker bridge.
//! Needs `python3`; the reference-parity test also needs the `yaml` module.
// A skipped test that says nothing looks like a passing one, so these report
// why they did not run.
#![allow(clippy::print_stderr)]

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use krab_inventory::dotkapitan::PythonResolverSettings;
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::resolvers::python::{PythonConfig, PythonResolvers};
use krab_inventory::{Inventory, InventoryConfig, Node, Registry, Value};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// `python3 <args> -c "import <modules>"` succeeds.
fn python_has(args: &[&str], modules: &str) -> bool {
    Command::new("python3")
        .args(args)
        .args(["-c", &format!("import {modules}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn registry_with(file: PathBuf, python: &str, prefer_native: bool) -> Registry {
    let cwd = file.parent().unwrap().to_path_buf();
    let settings = PythonResolverSettings {
        file: Some(file),
        python: Some(python.to_string()),
        prefer_native: Some(prefer_native),
        workers: Some(2),
        ..Default::default()
    };
    let cfg = PythonConfig::discover(&cwd, &cwd, &settings).expect("configured");
    let resolvers = PythonResolvers::new(cfg);
    let mut registry = Registry::with_builtins();
    let loaded = PythonResolvers::install(&resolvers, &mut registry)
        .unwrap_or_else(|e| panic!("install: {e}"));
    assert!(
        !loaded.resolvers.is_empty(),
        "pass_resolvers() returned nothing"
    );
    registry
}

fn at<'a>(node: &'a Node, path: &[&str]) -> &'a Node {
    path.iter().fold(node, |n, key| {
        n.get(key)
            .unwrap_or_else(|| panic!("no `{key}` in {}", n.value.py_repr()))
    })
}

fn check_small_fixture(python: &str) {
    let root = fixtures().join("python");
    let registry = registry_with(root.join("resolvers.py"), python, false);
    assert!(registry.contains("upper"));
    let inv = Inventory::new(
        InventoryConfig::new(root.join("inventory")),
        Arc::new(registry),
    );
    let target = inv
        .render_named("py")
        .unwrap_or_else(|e| panic!("render py: {e}"));
    let p = &target.parameters;
    let s = |path: &[&str]| at(p, path).value.py_str();
    assert_eq!(s(&["up"]), "HELLO");
    assert!(matches!(at(p, &["sum"]).value, Value::Int(6)));
    assert_eq!(s(&["padded"]), "hello...");
    assert_eq!(s(&["padded2"]), "hello-----");
    assert!(
        matches!(at(p, &["sib", "y"]).value, Value::Int(42)),
        "_parent_"
    );
    assert_eq!(
        s(&["root_b"]),
        "hello",
        "_root_ select of a nested interpolation"
    );
    assert_eq!(s(&["where"]), "where", "_node_._get_full_key");
    assert!(matches!(at(p, &["dumped", "a"]).value, Value::Int(1)));
    assert_eq!(s(&["dumped", "b"]), "hello", "to_container(resolve=True)");
    assert!(
        matches!(at(p, &["n"]).value, Value::Int(3)),
        "node list argument"
    );
    assert_eq!(
        s(&["types"]),
        "int-float-bool-NoneType-str",
        "literal types"
    );
    assert_eq!(s(&["dflt"]), "dflt", "select default");
    assert!(at(p, &["none"]).value.is_null(), "missing key selects None");

    let err = inv.render_named("err").err().expect("fail resolver raises");
    let msg = err.to_string();
    assert!(msg.contains("ValueError: boom"), "{msg}");
    assert!(msg.contains("`fail`"), "{msg}");

    let err = inv
        .render_named("nested_err")
        .err()
        .expect("nested resolution failure");
    let msg = err.to_string();
    assert!(msg.contains("KRAB_TEST_UNSET_VARIABLE_XYZ"), "{msg}");
    assert!(!msg.contains("Traceback"), "{msg}");
}

#[test]
fn small_fixture_with_the_installed_omegaconf() {
    if !python_has(&[], "sys") {
        eprintln!("python3 not available; skipping");
        return;
    }
    check_small_fixture("python3");
}

#[test]
fn small_fixture_with_the_stand_in_omegaconf() {
    // `-S` leaves site-packages out, so the worker's own `omegaconf` is used.
    if !python_has(&["-S"], "sys") || python_has(&["-S"], "omegaconf") {
        eprintln!("python3 -S not usable for the stand-in test; skipping");
        return;
    }
    check_small_fixture("python3 -S");
}

/// The contributed native set is a port of `tests/fixtures/inventory/resolvers.py`;
/// running that file through Python must reproduce the reference output too.
#[test]
fn fixture_inventory_matches_the_reference_through_python() {
    if !python_has(&[], "yaml") {
        eprintln!("python3 with PyYAML not available; skipping");
        return;
    }
    let root = fixtures().join("inventory");
    let registry = registry_with(root.join("resolvers.py"), "python3", false);
    let inv = Inventory::new(InventoryConfig::new(root), Arc::new(registry));
    let report = inv.render_all().expect("discover targets");
    if let Some(e) = report.errors.first() {
        panic!("{e}");
    }
    let mut checked = 0;
    for entry in std::fs::read_dir(fixtures().join("expected")).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        let expected = std::fs::read_to_string(&path).unwrap();
        let target = report
            .targets
            .get(&name)
            .unwrap_or_else(|| panic!("target {name} not rendered"));
        let actual = dump_yaml(&target.to_document(), &DumpOptions::default());
        assert!(
            actual == expected,
            "target {name} differs from the reference output when resolvers run in Python"
        );
        checked += 1;
    }
    assert!(checked >= 3);
}

#[test]
fn prefer_native_keeps_the_rust_resolver() {
    if !python_has(&[], "sys") {
        eprintln!("python3 not available; skipping");
        return;
    }
    // `replace` exists natively; the Python file redefines it. Python wins by
    // default (the reference behaviour), native wins with prefer-native.
    let dir = std::env::temp_dir().join(format!("krab-prefer-native-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join("resolvers.py"),
        "def replace(*a):\n    return 'python'\ndef pass_resolvers():\n    return {'replace': replace}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("inventory/targets/t.yml"),
        "parameters:\n  r: ${replace:abc,b,X}\n",
    )
    .unwrap();
    let render = |prefer_native: bool| {
        let registry = registry_with(dir.join("resolvers.py"), "python3", prefer_native);
        let inv = Inventory::new(
            InventoryConfig::new(dir.join("inventory")),
            Arc::new(registry),
        );
        let t = inv.render_named("t").unwrap_or_else(|e| panic!("{e}"));
        at(&t.parameters, &["r"]).value.py_str()
    };
    assert_eq!(render(false), "python");
    assert_eq!(render(true), "aXc");
    let _ = std::fs::remove_dir_all(dir);
}

/// A Python resolver whose `_root_` lookup evaluates another Python
/// resolver needs a second worker while holding the first. With a pool of
/// one that used to wait forever.
#[test]
fn nested_python_calls_do_not_deadlock() {
    if !python_has(&[], "sys") {
        eprintln!("python3 not available; skipping");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-nested-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join("resolvers.py"),
        "from omegaconf import OmegaConf\n\
         def outer(key, _root_):\n    return OmegaConf.select(_root_, key)\n\
         def inner(s):\n    return s.upper()\n\
         def pass_resolvers():\n    return {'outer': outer, 'inner': inner}\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("inventory/targets/t.yml"),
        "parameters:\n  c: ${outer:b}\n  b: ${outer:z.a}\n  z:\n    a: ${inner:hello}\n",
    )
    .unwrap();
    let settings = PythonResolverSettings {
        file: Some(dir.join("resolvers.py")),
        python: Some("python3".into()),
        workers: Some(1),
        ..Default::default()
    };
    let cfg = PythonConfig::discover(&dir, &dir, &settings).expect("configured");
    let resolvers = PythonResolvers::new(cfg);
    let mut registry = Registry::with_builtins();
    PythonResolvers::install(&resolvers, &mut registry).unwrap_or_else(|e| panic!("install: {e}"));
    let inv = Inventory::new(
        InventoryConfig::new(dir.join("inventory")),
        Arc::new(registry),
    );
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let t = inv
            .render_named("t")
            .map(|t| at(&t.parameters, &["c"]).value.py_str());
        let _ = tx.send(t);
    });
    let rendered = rx
        .recv_timeout(std::time::Duration::from_secs(90))
        .expect("render deadlocked on the nested Python call");
    assert_eq!(rendered.unwrap_or_else(|e| panic!("{e}")), "HELLO");
    let _ = std::fs::remove_dir_all(dir);
}

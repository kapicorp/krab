//! Renders the fixture inventory and compares the `kapitan inventory -t`
//! document with what kapitan 0.36.3 printed (`tests/fixtures/expected`).

use std::path::PathBuf;

use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::{Inventory, InventoryConfig};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures")
}

/// The expected documents are generated with `compose_target_name=True`
/// (`tests/fixtures/generate_expected.py`), so the fixture inventory is opened
/// the same way; the default is off, as in the reference.
fn fixture_inventory() -> Inventory {
    let mut cfg = InventoryConfig::new(fixtures().join("inventory"));
    cfg.compose_target_name = true;
    Inventory::new(
        cfg,
        std::sync::Arc::new(krab_inventory::resolvers::Registry::with_builtins()),
    )
}

#[test]
fn renders_like_the_reference() {
    let inv = fixture_inventory();
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
        if actual != expected {
            let diff: Vec<String> = expected
                .lines()
                .zip(actual.lines())
                .enumerate()
                .filter(|(_, (a, b))| a != b)
                .take(5)
                .map(|(i, (a, b))| format!("line {}:\n  expected: {a}\n  actual:   {b}", i + 1))
                .collect();
            panic!(
                "target {name} differs from the reference output:\n{}",
                diff.join("\n")
            );
        }
        checked += 1;
    }
    assert!(checked >= 3, "expected fixtures present");
}

#[test]
fn explain_reports_overrides() {
    let inv = fixture_inventory();
    let target = inv.render_named("env.prod").unwrap();
    let e = krab_inventory::explain::explain(&inv, &target, "database.engine").unwrap();
    assert_eq!(e.value.py_str(), "mysql");
    assert!(
        e.origin
            .as_ref()
            .unwrap()
            .file
            .ends_with("targets/env/prod.yml")
    );
    assert_eq!(e.history.len(), 1, "{:?}", e.history);
    let e = krab_inventory::explain::explain(&inv, &target, "database.hosts").unwrap();
    assert!(
        e.history.len() >= 2,
        "list appends recorded: {:?}",
        e.history
    );
    let e = krab_inventory::explain::explain(&inv, &target, "app.hostname").unwrap();
    assert!(e.resolved_from.is_some());
}

#[test]
fn single_target_render_touches_only_its_closure() {
    let inv = fixture_inventory();
    let target = inv.render_named("bare").unwrap();
    let files: Vec<String> = target
        .files
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(files, vec!["bare.yml", "common.yml", "empty.yml"]);
    assert!(
        target
            .probes
            .iter()
            .any(|p| p.ends_with("classes/common/init.yml")),
        "misses are recorded"
    );
}

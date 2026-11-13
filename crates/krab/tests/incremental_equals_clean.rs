//! An incremental compile writes what a clean compile writes (CMP-60).
//!
//! Each step changes the repository the way a user does, compiles it
//! incrementally, and compares `compiled/` byte for byte with a clean compile
//! of a fresh copy. A reuse decision that misses a change shows up as a
//! difference here, whichever rule it broke. New cases are new steps.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Run `krab compile` in `dir` and return its stderr.
fn compile(dir: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stderr}");
    stderr
}

/// Every file under `dir`, relative path to content, the manifest left out.
fn tree(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(root, &path, out);
            } else if entry.file_name() != ".krab-manifest.json" {
                let rel = path.strip_prefix(root).unwrap().to_path_buf();
                out.insert(rel, std::fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    if dir.is_dir() {
        walk(dir, dir, &mut out);
    }
    out
}

fn copy_source(from: &Path, to: &Path) {
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let name = entry.file_name();
        if name == "compiled" {
            continue;
        }
        let dest = to.join(&name);
        if entry.path().is_dir() {
            std::fs::create_dir_all(&dest).unwrap();
            copy_source(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).unwrap();
        }
    }
}

/// Compile `repo` incrementally and a fresh copy of it from scratch; the two
/// `compiled/` trees must be equal.
fn assert_incremental_equals_clean(repo: &Path, step: &str) {
    compile(repo, &[]);
    let clean = repo.with_extension(format!("clean-{}", step.replace(' ', "-")));
    let _ = std::fs::remove_dir_all(&clean);
    std::fs::create_dir_all(&clean).unwrap();
    copy_source(repo, &clean);
    compile(&clean, &[]);
    let (incremental, expected) = (tree(&repo.join("compiled")), tree(&clean.join("compiled")));
    let _ = std::fs::remove_dir_all(&clean);
    let differ: Vec<&PathBuf> = incremental
        .keys()
        .chain(expected.keys())
        .filter(|k| incremental.get(*k) != expected.get(*k))
        .collect();
    assert!(
        differ.is_empty(),
        "after `{step}` the incremental compile differs from a clean one in {differ:?}"
    );
}

const COMPONENT: &str = "from kapitan.inputs.kadet import BaseObj\n\n\ndef main(input_params):\n    obj = BaseObj()\n    obj.root.NAME = {\"name\": \"NAME\", \"version\": 1}\n    return obj\n";

#[test]
fn incremental_compiles_equal_clean_ones() {
    let base = std::env::temp_dir().join(format!("krab-incr-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let repo = base.join("repo");
    write(
        &repo.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    );
    for name in ["b", "c"] {
        write(
            &repo.join(format!("components/gen/{name}/__init__.py")),
            &COMPONENT.replace("NAME", name),
        );
    }
    write(
        &repo.join("components/watch/__init__.py"),
        "from kapitan.inputs.kadet import BaseObj, inventory_global\n\n\ndef main(input_params):\n    obj = BaseObj()\n    obj.root.watch = {\"monitoring\": \"monitoring\" in inventory_global()}\n    return obj\n",
    );
    write(&repo.join("templates/a.txt"), "a\n");
    write(
        &repo.join("inventory/targets/k.yml"),
        "parameters:\n  kapitan:\n    vars:\n      target: k\n    compile:\n      - input_type: kadet\n        input_paths: [components/gen/*]\n        output_path: gen\n      - input_type: kadet\n        input_paths: [components/watch]\n        output_path: watch\n",
    );
    write(
        &repo.join("inventory/targets/t1.yml"),
        "parameters:\n  kapitan:\n    vars:\n      target: t1\n    compile:\n      - input_type: copy\n        input_paths: [templates/a.txt]\n        output_path: .\n",
    );
    assert_incremental_equals_clean(&repo, "first compile");

    // A component the glob did not match before.
    write(
        &repo.join("components/gen/a/__init__.py"),
        &COMPONENT.replace("NAME", "a"),
    );
    assert_incremental_equals_clean(&repo, "add a component under a glob");

    // A target that a component looked up while it did not exist.
    write(
        &repo.join("inventory/targets/monitoring.yml"),
        "parameters:\n  kapitan:\n    vars:\n      target: monitoring\n    compile:\n      - input_type: copy\n        input_paths: [templates/a.txt]\n        output_path: .\n",
    );
    assert_incremental_equals_clean(&repo, "add a target another one looked up");

    write(
        &repo.join("components/gen/b/__init__.py"),
        &COMPONENT.replace("NAME", "b").replace("1}", "2}"),
    );
    assert_incremental_equals_clean(&repo, "change a component");

    // A checkout in another directory keeps its records.
    let moved = base.join("moved");
    std::fs::rename(&repo, &moved).unwrap();
    let stderr = compile(&moved, &[]);
    assert!(
        stderr
            .lines()
            .any(|l| l.starts_with("all ") && l.ends_with("targets up to date")),
        "moving the checkout recompiled: {stderr}"
    );
    assert_incremental_equals_clean(&moved, "move the checkout");
    let _ = std::fs::remove_dir_all(&base);
}

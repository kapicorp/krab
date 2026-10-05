//! `input_type: helm` compiles natively and matches kapitan 0.36.3 on the
//! fixture chart (`tests/fixtures/helm`, expected output in
//! `tests/fixtures/helm-expected/compiled`). Needs a `helm` v3 binary; skips
//! without one. In CI it fails instead on Linux, whose GitHub runner image
//! ships helm; the macOS image has none, so it still skips there.
#![allow(clippy::print_stderr)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn have_helm() -> bool {
    Command::new("helm")
        .args(["version", "--short"])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// Report why the test does not run. In CI on Linux a missing helm is a
/// failure, so a broken runner cannot turn the test into a silent pass.
fn skip(why: &str) {
    assert!(
        std::env::var_os("CI").is_none() || !cfg!(target_os = "linux"),
        "{why}, and CI must run this test"
    );
    eprintln!("{why}; skipping");
}

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

fn files(root: &Path, dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(root, &path, out);
        } else if path.file_name().is_some_and(|n| n != ".krab-manifest.json") {
            out.push(path.strip_prefix(root).unwrap().to_path_buf());
        }
    }
}

#[test]
fn helm_input_matches_the_reference() {
    if !have_helm() {
        skip("helm not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-helm-input-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&fixtures().join("helm"), &dir);

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

    let expected = fixtures().join("helm-expected/compiled");
    let actual = dir.join("compiled");
    let (mut want, mut got) = (Vec::new(), Vec::new());
    files(&expected, &expected, &mut want);
    files(&actual, &actual, &mut got);
    want.sort();
    got.sort();
    assert_eq!(got, want, "the same files");
    for rel in &want {
        assert_eq!(
            std::fs::read_to_string(actual.join(rel)).unwrap(),
            std::fs::read_to_string(expected.join(rel)).unwrap(),
            "{}",
            rel.display()
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

fn compile(dir: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["--no-daemon", "compile"])
        .current_dir(dir)
        .output()
        .unwrap()
}

#[test]
fn an_edited_chart_template_makes_the_target_stale() {
    if !have_helm() {
        skip("helm not available");
        return;
    }
    let dir = std::env::temp_dir().join(format!("krab-helm-stale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&fixtures().join("helm"), &dir);
    assert!(compile(&dir).status.success());
    let again = String::from_utf8_lossy(&compile(&dir).stderr).to_string();
    assert!(again.contains("0 compiled"), "{again}");

    let template = dir.join("charts/demo2/templates/cm.yaml");
    let text = std::fs::read_to_string(&template).unwrap();
    std::fs::write(&template, text.replace("-demo2", "-edited")).unwrap();
    let out = String::from_utf8_lossy(&compile(&dir).stderr).to_string();
    assert!(out.contains("1 compiled"), "{out}");
    let rendered =
        std::fs::read_to_string(dir.join("compiled/charts/rendered/demo2/templates/cm.yaml"))
            .unwrap();
    assert!(rendered.contains("release-name-edited"), "{rendered}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_missing_helm_binary_is_reported_like_the_reference() {
    let dir = std::env::temp_dir().join(format!("krab-helm-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&fixtures().join("helm"), &dir);
    let target = dir.join("inventory/targets/charts.yml");
    let text = std::fs::read_to_string(&target).unwrap();
    std::fs::write(
        &target,
        text.replace(
            "        kube_version: \"1.29\"\n",
            "        kube_version: \"1.29\"\n        helm_path: /nonexistent/helm\n",
        ),
    )
    .unwrap();
    let out = compile(&dir);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("helm binary not found. helm must be present in the PATH"),
        "{stderr}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

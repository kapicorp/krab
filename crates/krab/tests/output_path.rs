//! `output_path: .` writes an input's files directly into `compiled/<target>/`.

use std::process::Command;

#[test]
fn output_path_dot_is_the_target_directory() {
    let dir = std::env::temp_dir().join(format!("krab-output-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(dir.join("templates/x.yml"), "a: 1\n").unwrap();
    std::fs::write(
        dir.join("inventory/targets/t1.yml"),
        "parameters:\n  kapitan:\n    vars:\n      target: t1\n    compile:\n      - input_type: copy\n        input_paths:\n          - templates/x.yml\n        output_path: .\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["compile", "--no-daemon"])
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("compiled/t1/x.yml")).unwrap(),
        "a: 1\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// An `output_path` that leaves `compiled/<target>/` fails the target instead
/// of being dropped with its temporary tree (#133), and writes nothing.
#[test]
fn output_path_outside_the_target_directory_fails() {
    for (case, output_path) in [("sibling", "../shared"), ("root", "../../../escape")] {
        let dir =
            std::env::temp_dir().join(format!("krab-output-outside-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
        std::fs::create_dir_all(dir.join("templates")).unwrap();
        std::fs::write(
            dir.join(".kapitan"),
            "global:\n  inventory-backend: omegaconf\n",
        )
        .unwrap();
        std::fs::write(dir.join("templates/x.yml"), "a: 1\n").unwrap();
        std::fs::write(
            dir.join("inventory/targets/t1.yml"),
            format!("parameters:\n  kapitan:\n    vars:\n      target: t1\n    compile:\n      - input_type: copy\n        input_paths:\n          - templates/x.yml\n        output_path: {output_path}\n"),
        )
        .unwrap();

        let out = Command::new(env!("CARGO_BIN_EXE_krab"))
            .args(["compile", "--no-daemon"])
            .current_dir(&dir)
            .output()
            .unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "{case}: {stderr}");
        assert!(stderr.contains(output_path), "{case}: {stderr}");
        assert!(!dir.join("compiled/shared").exists(), "{case}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// A target name starting with a dot gives an absolute target path
/// (`.tmp.x` is `/tmp/x`). The target fails before anything is written or
/// removed there (CMP-58); before, the directory was emptied.
#[test]
fn a_target_path_outside_compiled_fails_without_touching_it() {
    let victim_name = format!("krabprobe{}", std::process::id());
    let victim = std::path::PathBuf::from("/tmp").join(&victim_name);
    let _ = std::fs::remove_dir_all(&victim);
    std::fs::create_dir_all(&victim).unwrap();
    std::fs::write(victim.join("keep.txt"), "keep\n").unwrap();

    let dir = std::env::temp_dir().join(format!("krab-target-path-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(dir.join("templates/a.txt"), "a\n").unwrap();
    std::fs::write(
        dir.join(format!("inventory/targets/.tmp.{victim_name}.yml")),
        "parameters:\n  kapitan:\n    vars:\n      target: x\n    compile:\n      - input_type: copy\n        input_paths: [templates/a.txt]\n        output_path: .\n",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["compile", "--no-daemon"])
        .current_dir(&dir)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    let kept = victim.join("keep.txt").is_file();
    let written = victim.join("a.txt").exists();
    let _ = std::fs::remove_dir_all(&victim);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        kept,
        "the directory outside compiled/ was emptied: {stderr}"
    );
    assert!(!written, "wrote outside compiled/: {stderr}");
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains(&victim_name), "{stderr}");
}

//! `remove` deletes in the target's own build tree (OUT-25) and leaves
//! installed output under `compiled/` alone (OUT-25a).

use std::path::Path;
use std::process::Command;

fn compile(dir: &Path, args: &[&str]) {
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args(["compile", "--no-daemon"])
        .args(args)
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
fn remove_deletes_only_in_the_targets_own_build_tree() {
    let dir = std::env::temp_dir().join(format!("krab-remove-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::create_dir_all(dir.join("templates")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(dir.join("templates/a.txt"), "a\n").unwrap();
    std::fs::write(dir.join("templates/b.txt"), "b\n").unwrap();
    let copy = "      - input_type: copy\n        input_paths: [templates/a.txt, templates/b.txt]\n        output_path: .\n";
    std::fs::write(
        dir.join("inventory/targets/t1.yml"),
        format!("parameters:\n  kapitan:\n    vars:\n      target: t1\n    compile:\n{copy}"),
    )
    .unwrap();
    // t2 removes one of its own files, and names one of t1's. Whether t1's
    // file exists while t2 compiles depends on the order, hence ignore_missing.
    std::fs::write(
        dir.join("inventory/targets/t2.yml"),
        format!(
            "parameters:\n  kapitan:\n    vars:\n      target: t2\n    compile:\n{copy}      - input_type: remove\n        input_paths: [compiled/t2/b.txt, compiled/t1/a.txt]\n        ignore_missing: true\n        output_path: .\n"
        ),
    )
    .unwrap();

    // t1 first, so its file is installed when t2 compiles; a full parallel
    // run would leave the order to chance.
    compile(&dir, &["-t", "t1"]);
    compile(&dir, &["-t", "t2"]);
    assert!(dir.join("compiled/t2/a.txt").is_file());
    assert!(
        !dir.join("compiled/t2/b.txt").exists(),
        "own file not removed"
    );
    assert!(
        dir.join("compiled/t1/a.txt").is_file(),
        "compiling t2 removed t1's installed file"
    );

    compile(&dir, &["--force"]);
    assert!(
        dir.join("compiled/t1/a.txt").is_file(),
        "a full run removed it"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

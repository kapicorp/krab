//! A `.kapitan` `version:` that kapitan 0.36.3 does not satisfy stops
//! `krab compile`, as it stops the reference, unless the check is skipped.

use std::process::Command;

#[test]
fn compile_refuses_a_version_pin_it_does_not_match() {
    let dir = std::env::temp_dir().join(format!("krab-version-check-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join("inventory/targets/t1.yml"),
        "parameters:\n  a: 1\n",
    )
    .unwrap();
    let compile = |kapitan: &str, args: &[&str]| {
        std::fs::write(dir.join(".kapitan"), kapitan).unwrap();
        Command::new(env!("CARGO_BIN_EXE_krab"))
            .args(["compile", "--no-daemon"])
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap()
    };
    let pinned = "version: 99.0\nglobal:\n  inventory-backend: omegaconf\n";

    let out = compile(pinned, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("kapitan 99.0"), "{stderr}");

    let out = compile(pinned, &["--ignore-version-check"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let out = compile(
        &format!("{pinned}compile:\n  ignore-version-check: true\n"),
        &[],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

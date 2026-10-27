//! `--json` on `krab inventory -t X` prints the target document as JSON.

use std::process::Command;

#[test]
fn json_flag_prints_the_target_document_as_json() {
    let dir = std::env::temp_dir().join(format!("krab-inventory-json-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    std::fs::write(dir.join("inventory/targets/t.yml"), "parameters:\n  a: 1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_krab"))
        .args([
            "--no-daemon",
            "--json",
            "inventory",
            "-t",
            "t",
            "-p",
            "parameters",
        ])
        .current_dir(&dir)
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let doc: serde_json::Value = serde_json::from_str(&stdout).expect(&stdout);
    assert_eq!(doc["a"], 1, "{stdout}");
}

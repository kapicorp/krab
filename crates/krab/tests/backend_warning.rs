//! krab warns once on stderr when kapitan would not render the inventory
//! with the omegaconf backend.

use std::process::Command;

#[test]
fn warns_when_the_backend_is_not_omegaconf() {
    let dir = std::env::temp_dir().join(format!("krab-backend-warning-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("inventory/targets")).unwrap();
    std::fs::write(dir.join("inventory/targets/t.yml"), "parameters: {}\n").unwrap();
    let stderr = || {
        let out = Command::new(env!("CARGO_BIN_EXE_krab"))
            .args(["--no-daemon", "inventory", "targets"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8_lossy(&out.stderr).to_string()
    };
    let warned = stderr();
    assert_eq!(warned.matches("warning:").count(), 1, "{warned}");
    assert!(warned.contains("inventory-backend: omegaconf"), "{warned}");

    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\n",
    )
    .unwrap();
    let quiet = stderr();
    assert!(!quiet.contains("warning:"), "{quiet}");

    std::fs::write(
        dir.join(".kapitan"),
        "global:\n  inventory-backend: omegaconf\ncompile:\n  prnue: true\n",
    )
    .unwrap();
    let typo = stderr();
    assert!(
        typo.contains("warning: `.kapitan`: unknown key `compile.prnue`"),
        "{typo}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

//! The runtime directory is 0700 and the socket 0600 whatever the umask
//! (SRV-14), and the daemon refuses a runtime directory that is not a
//! directory of its own user (SRV-14a).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use krab_inventory::resolvers::Registry;
use krab_inventory::{Inventory, InventoryConfig};
use krab_server::{Server, ServerConfig, State};

fn server(socket: PathBuf) -> Server {
    let inventory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/inventory");
    Server {
        state: Arc::new(State::new(Inventory::new(
            InventoryConfig::new(inventory),
            Arc::new(Registry::with_builtins()),
        ))),
        cfg: ServerConfig {
            socket,
            log: None,
            idle_timeout: Duration::from_secs(1),
            version: "test".into(),
        },
        shutdown: AtomicBool::new(false),
    }
}

fn mode(path: &Path) -> String {
    format!(
        "{:o}",
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    )
}

fn scratch(name: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!("krab-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    base
}

#[test]
fn the_runtime_directory_and_the_socket_are_private_whatever_the_umask() {
    // umask 002 would leave both group-writable. An older krab created the
    // directory 0755 under the usual 022.
    // SAFETY: umask() takes a mode and cannot fail.
    unsafe { libc::umask(0o002) };
    let base = scratch("perm-mode");
    let dir = base.join("krab");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    let socket = dir.join("t.sock");

    let _listener = server(socket.clone()).bind().unwrap();

    assert_eq!(mode(&dir), "700", "runtime directory");
    assert_eq!(mode(&socket), "600", "socket");
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn a_runtime_directory_that_is_not_its_own_is_refused() {
    let base = scratch("perm-symlink");
    let elsewhere = base.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    let dir = base.join("krab");
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();

    let err = server(dir.join("t.sock")).bind().unwrap_err();

    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied, "{err}");
    assert!(
        err.to_string().contains(&dir.display().to_string()),
        "{err}"
    );
    assert!(
        !elsewhere.join("t.sock").exists(),
        "bound through the symlink"
    );
    std::fs::remove_dir_all(&base).unwrap();
}

//! Where a server for a given inventory lives: socket and log file.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::os::unix::net::{SocketAddr, UnixStream};
use std::path::{Path, PathBuf};

fn inventory_key(inventory_root: &Path) -> String {
    let canonical = inventory_root
        .canonicalize()
        .unwrap_or_else(|_| inventory_root.to_path_buf());
    blake3::hash(canonical.to_string_lossy().as_bytes()).to_hex()[..16].to_string()
}

fn runtime_dir() -> PathBuf {
    runtime_dir_in(std::env::var("XDG_RUNTIME_DIR").ok())
}

fn runtime_dir_in(xdg_runtime_dir: Option<String>) -> PathBuf {
    if let Some(dir) = xdg_runtime_dir
        && !dir.is_empty()
    {
        let dir = PathBuf::from(dir).join("krab");
        // Every socket name has this length. A directory too long to hold it
        // would leave the daemon unable to bind, so use the short one below.
        if SocketAddr::from_pathname(dir.join("0000000000000000-00000000.sock")).is_ok() {
            return dir;
        }
    }
    // SAFETY: getuid() takes no arguments, cannot fail and has no preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/krab-{uid}"))
}

/// Create the runtime directory with mode 0700, or check the one that exists
/// (SRV-14, SRV-14a). Whoever controls the directory can put their own
/// socket in it, so a symlink or a directory of another user is refused.
/// A directory of ours with another mode, such as one an older krab created
/// under the umask, is set to 0700.
pub fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    let meta = std::fs::symlink_metadata(dir)?;
    // SAFETY: getuid() takes no arguments, cannot fail and has no preconditions.
    let uid = unsafe { libc::getuid() };
    if !meta.is_dir() || meta.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "runtime directory {} is not a directory owned by uid {uid}; \
                 remove it or set XDG_RUNTIME_DIR",
                dir.display()
            ),
        ));
    }
    if meta.mode() & 0o777 != 0o700 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn state_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_STATE_HOME")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("krab");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local/state/krab")
}

/// `<inventory>-<build>.sock`: one socket per inventory *and* build. Two
/// builds pointed at the same inventory each keep their own server instead
/// of restarting each other's, and a rebuilt binary gets a fresh socket
/// while the previous server idles out.
pub fn socket_path(inventory_root: &Path, build: &str) -> PathBuf {
    let build = &blake3::hash(build.as_bytes()).to_hex()[..8];
    runtime_dir().join(format!("{}-{build}.sock", inventory_key(inventory_root)))
}

/// Every socket of this inventory in the runtime directory, whichever build
/// made it: the ones a server answers on, and the dead ones left behind.
pub fn sockets(inventory_root: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let prefix = format!("{}-", inventory_key(inventory_root));
    let (mut live, mut dead) = (Vec::new(), Vec::new());
    let dir = runtime_dir();
    if ensure_private_dir(&dir).is_err() {
        return (live, dead);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (live, dead);
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with(&prefix) || !name.ends_with(".sock") {
            continue;
        }
        let path = entry.path();
        if UnixStream::connect(&path).is_ok() {
            live.push(path);
        } else {
            dead.push(path);
        }
    }
    live.sort();
    dead.sort();
    (live, dead)
}

pub fn log_path(inventory_root: &Path) -> PathBuf {
    state_dir().join(format!("server-{}.log", inventory_key(inventory_root)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_runtime_dir_still_gives_a_bindable_socket() {
        let long = format!("/tmp/{}", "x".repeat(80));
        let dir = runtime_dir_in(Some(long));
        let socket = dir.join(format!("{}-{}.sock", "0".repeat(16), "0".repeat(8)));
        assert!(
            SocketAddr::from_pathname(&socket).is_ok(),
            "{} is too long for a unix socket",
            socket.display()
        );
        let short = runtime_dir_in(Some("/run/user/1000".into()));
        assert_eq!(short, PathBuf::from("/run/user/1000/krab"));
    }
}

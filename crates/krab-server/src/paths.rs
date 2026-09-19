//! Where a server for a given inventory lives: socket and log file.

use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

fn inventory_key(inventory_root: &Path) -> String {
    let canonical = inventory_root
        .canonicalize()
        .unwrap_or_else(|_| inventory_root.to_path_buf());
    blake3::hash(canonical.to_string_lossy().as_bytes()).to_hex()[..16].to_string()
}

fn runtime_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir).join("krab");
    }
    // SAFETY: getuid() takes no arguments, cannot fail and has no preconditions.
    let uid = unsafe { libc::getuid() };
    PathBuf::from(format!("/tmp/krab-{uid}"))
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
    let Ok(entries) = std::fs::read_dir(runtime_dir()) else {
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

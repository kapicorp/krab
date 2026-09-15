//! Content digests of files and directory listings, memoised per run.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

/// `f:<hex>` for a file's content and mode, `d:<hex>` for a directory listing,
/// `-` for a path that does not exist.
pub type Fingerprint = String;

#[derive(Default)]
pub struct Digests {
    memo: Mutex<HashMap<PathBuf, Fingerprint>>,
}

impl Digests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a fingerprint known from elsewhere (the bytes just written),
    /// so `fingerprint` and `tree` need not read the file back.
    pub fn seed(&self, path: PathBuf, fp: Fingerprint) {
        self.memo.lock().insert(path, fp);
    }

    /// Fingerprint a regular file would get with this content and exec bit.
    pub fn of_bytes(exec: bool, bytes: &[u8]) -> Fingerprint {
        let mut h = blake3::Hasher::new();
        h.update(if exec { b"x" } else { b"-" });
        h.update(bytes);
        format!("f:{}", h.finalize().to_hex())
    }

    /// Fingerprint of whatever is at `path` now (file, directory or nothing).
    pub fn fingerprint(&self, path: &Path) -> Fingerprint {
        if let Some(f) = self.memo.lock().get(path) {
            return f.clone();
        }
        let f = compute(path);
        self.memo.lock().insert(path.to_path_buf(), f.clone());
        f
    }

    /// Digest of a whole directory tree: relative paths, modes and contents.
    /// `skip` names top-level entries to leave out (nested targets).
    pub fn tree(&self, root: &Path, skip: &[String]) -> String {
        let mut files = Vec::new();
        collect(root, root, skip, &mut files);
        files.sort();
        let mut h = blake3::Hasher::new();
        for rel in files {
            let full = root.join(&rel);
            h.update(rel.as_bytes());
            h.update(b"\0");
            h.update(self.fingerprint(&full).as_bytes());
            h.update(b"\0");
        }
        h.finalize().to_hex().to_string()
    }
}

fn compute(path: &Path) -> Fingerprint {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return "-".into();
    };
    if meta.is_dir() {
        let mut names: Vec<String> = match std::fs::read_dir(path) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .map(|e| {
                    let kind = if e.path().is_dir() { "/" } else { "" };
                    format!("{}{kind}", e.file_name().to_string_lossy())
                })
                .collect(),
            Err(_) => return "-".into(),
        };
        names.sort();
        let mut h = blake3::Hasher::new();
        for n in names {
            h.update(n.as_bytes());
            h.update(b"\0");
        }
        return format!("d:{}", h.finalize().to_hex());
    }
    let Ok(bytes) = std::fs::read(path) else {
        return "-".into();
    };
    // Only the executable bit matters for outputs (jinja2 preserves it).
    #[cfg(unix)]
    let exec = {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    };
    #[cfg(not(unix))]
    let exec = false;
    Digests::of_bytes(exec, &bytes)
}

fn collect(root: &Path, dir: &Path, skip: &[String], out: &mut Vec<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        if dir == root
            && skip
                .iter()
                .any(|s| entry.file_name().to_string_lossy() == s.as_str())
        {
            continue;
        }
        if path.is_dir() {
            collect(root, &path, skip, out);
        } else if let Ok(rel) = path.strip_prefix(root) {
            out.push(
                rel.to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, "/"),
            );
        }
    }
}

pub fn digest_str(s: &str) -> String {
    blake3::hash(s.as_bytes()).to_hex().to_string()
}

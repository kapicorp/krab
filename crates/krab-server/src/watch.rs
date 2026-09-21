//! File watching: debounce filesystem events under the inventory directory
//! (and under the real location of symlinked files) and feed them to the state.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode};
use notify_debouncer_full::{DebounceEventResult, Debouncer, RecommendedCache, new_debouncer};
use parking_lot::Mutex;

use crate::state::State;

pub type Handle = Arc<Mutex<Debouncer<notify::RecommendedWatcher, RecommendedCache>>>;

/// Start watching `root`. The returned handle keeps the watcher alive; a
/// helper thread adds watches for symlink targets as they are discovered.
pub fn start(root: &Path, state: Arc<State>) -> notify::Result<Handle> {
    let cb_state = state.clone();
    let mut debouncer = new_debouncer(
        Duration::from_millis(150),
        None,
        move |result: DebounceEventResult| {
            let events = match result {
                Ok(events) => events,
                Err(errors) => {
                    for e in errors {
                        tracing::warn!("watch error: {e}");
                    }
                    return;
                }
            };
            let mut changed: Vec<PathBuf> = Vec::new();
            for event in events {
                tracing::debug!(kind = ?event.kind, paths = ?event.paths, "fs event");
                if matches!(event.kind, EventKind::Access(_)) {
                    continue;
                }
                for path in &event.paths {
                    // Existing non-inventory files (editor swap files, READMEs) are
                    // noise; directories and vanished paths may hide relevant files.
                    if path.is_file()
                        && !State::is_inventory_file(path)
                        && !cb_state.is_registry_source(path)
                    {
                        continue;
                    }
                    if !changed.contains(path) {
                        changed.push(path.clone());
                    }
                }
            }
            if changed.is_empty() {
                return;
            }
            let summary = cb_state.apply_changes(changed);
            tracing::info!(
                generation = summary.generation,
                files = summary.changed_files.len(),
                rerendered = summary.rerendered.len(),
                errors = summary.errors.len(),
                ms = summary.duration_ms,
                "re-rendered"
            );
        },
    )?;
    debouncer.watch(root, RecursiveMode::Recursive)?;
    // Files the server was configured from (`.kapitan`, a `resolvers.py`) may
    // live outside the inventory; watch their directories so a change restarts us.
    for dir in state
        .inv
        .registry
        .sources()
        .iter()
        .filter_map(|f| f.parent())
        .filter(|d| !d.starts_with(root))
    {
        if let Err(e) = debouncer.watch(dir, RecursiveMode::NonRecursive) {
            tracing::warn!("cannot watch {}: {e}", dir.display());
        }
    }
    let handle = Arc::new(Mutex::new(debouncer));

    // Symlinked files live elsewhere; watch their real directories too.
    let root = root.to_path_buf();
    let alias_handle = handle.clone();
    std::thread::spawn(move || {
        let mut watched: BTreeSet<PathBuf> = BTreeSet::new();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            for dir in state.alias_dirs() {
                if dir.starts_with(&root) || watched.contains(&dir) {
                    continue;
                }
                match alias_handle.lock().watch(&dir, RecursiveMode::NonRecursive) {
                    Ok(()) => {
                        tracing::info!(dir = %dir.display(), "watching symlink target directory");
                        watched.insert(dir);
                    }
                    Err(e) => tracing::warn!("cannot watch {}: {e}", dir.display()),
                }
            }
        }
    });
    Ok(handle)
}

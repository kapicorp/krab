//! The in-memory inventory: rendered targets, a path → targets index, and
//! incremental re-rendering when files change.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use krab_inventory::{Diagnostic, Inventory, RenderedTarget, TargetSpec};
use parking_lot::{Condvar, Mutex, RwLock};

use crate::protocol::ChangeSummary;

pub struct State {
    pub inv: Inventory,
    inner: RwLock<Inner>,
    changed: Condvar,
    changed_lock: Mutex<()>,
    pub started: Instant,
    pub last_request: Mutex<Instant>,
    /// Set when the server must exit and be started afresh (a file it was
    /// configured from changed: `.kapitan`, a `resolvers.py`).
    pub stop: AtomicBool,
    /// Set once the initial render is done; requests wait for it.
    ready: AtomicBool,
    ready_cv: Condvar,
    ready_lock: Mutex<()>,
}

#[derive(Default)]
pub struct Inner {
    pub specs: Vec<TargetSpec>,
    pub targets: BTreeMap<String, Arc<RenderedTarget>>,
    pub errors: BTreeMap<String, Diagnostic>,
    /// Which targets a path matters to: files they were rendered from and
    /// paths probed while resolving their class names.
    pub index: HashMap<PathBuf, BTreeSet<String>>,
    /// Real location of symlinked inventory files → the paths the inventory
    /// knows them by. The watcher watches the real locations too.
    pub aliases: HashMap<PathBuf, BTreeSet<PathBuf>>,
    pub generation: u64,
    pub history: VecDeque<ChangeSummary>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl State {
    pub fn new(inv: Inventory) -> Self {
        State {
            inv,
            inner: RwLock::new(Inner::default()),
            changed: Condvar::new(),
            changed_lock: Mutex::new(()),
            started: Instant::now(),
            last_request: Mutex::new(Instant::now()),
            stop: AtomicBool::new(false),
            ready: AtomicBool::new(false),
            ready_cv: Condvar::new(),
            ready_lock: Mutex::new(()),
        }
    }

    /// The initial render is done; `inventory.*` requests may be answered.
    pub fn set_ready(&self) {
        let _guard = self.ready_lock.lock();
        self.ready.store(true, Ordering::SeqCst);
        self.ready_cv.notify_all();
    }

    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Block until the initial render is done.
    pub fn wait_ready(&self) {
        let mut guard = self.ready_lock.lock();
        while !self.ready.load(Ordering::SeqCst) {
            self.ready_cv.wait(&mut guard);
        }
    }

    /// A file the server was configured from (`.kapitan`, a `resolvers.py`
    /// and the modules it imports).
    pub fn is_registry_source(&self, path: &Path) -> bool {
        self.inv.registry.is_source(path)
    }

    pub fn should_stop(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn read(&self) -> parking_lot::RwLockReadGuard<'_, Inner> {
        self.inner.read()
    }

    pub fn touch(&self) {
        *self.last_request.lock() = Instant::now();
    }

    pub fn idle_for(&self) -> Duration {
        self.last_request.lock().elapsed()
    }

    /// Directories outside the inventory that hold real files behind symlinks.
    pub fn alias_dirs(&self) -> BTreeSet<PathBuf> {
        self.inner
            .read()
            .aliases
            .keys()
            .filter_map(|p| p.parent().map(Path::to_path_buf))
            .collect()
    }

    /// Render every target from scratch.
    pub fn render_all(&self) -> ChangeSummary {
        self.inv.invalidate_all();
        let specs = self.inv.discover_targets().unwrap_or_default();
        let all: BTreeSet<String> = specs.iter().map(|s| s.name.clone()).collect();
        self.rerender(Vec::new(), specs, all)
    }

    /// Files changed on disk (created, modified, renamed or removed): drop the
    /// caches that depend on them and re-render exactly the affected targets.
    pub fn apply_changes(&self, changed: Vec<PathBuf>) -> ChangeSummary {
        if let Some(source) = changed.iter().find(|p| self.is_registry_source(p)) {
            // The configuration (`.kapitan`, Python resolvers) is fixed for the
            // life of the process; the next client request starts a fresh server.
            tracing::info!(file = %source.display(), "configuration source changed, restarting");
            self.stop.store(true, Ordering::SeqCst);
        }
        let changed = self.expand_aliases(changed);
        self.inv.invalidate_dependents(&changed);
        for dir in changed.iter().filter(|p| !p.is_file()) {
            self.inv.invalidate_under(dir);
        }
        let specs = self.inv.discover_targets().unwrap_or_default();
        let mut affected: BTreeSet<String> = BTreeSet::new();
        {
            let inner = self.inner.read();
            for path in &changed {
                if let Some(names) = inner.index.get(path) {
                    affected.extend(names.iter().cloned());
                }
                if !path.is_file() {
                    // A directory (or something that no longer exists): every
                    // indexed path underneath it is affected.
                    for (indexed, names) in &inner.index {
                        if indexed.starts_with(path) {
                            affected.extend(names.iter().cloned());
                        }
                    }
                }
            }
            // Failed renders have no complete dependency list: retry them all.
            tracing::debug!(changed = ?changed, from_index = affected.len(), failed = inner.errors.len(), "apply_changes");
            affected.extend(inner.errors.keys().cloned());
            // New target files.
            let known: BTreeSet<&String> = inner.specs.iter().map(|s| &s.name).collect();
            affected.extend(
                specs
                    .iter()
                    .filter(|s| !known.contains(&s.name))
                    .map(|s| s.name.clone()),
            );
        }
        self.rerender(changed, specs, affected)
    }

    fn expand_aliases(&self, changed: Vec<PathBuf>) -> Vec<PathBuf> {
        let inner = self.inner.read();
        let mut out = Vec::new();
        for p in changed {
            if let Some(aliases) = inner.aliases.get(&p) {
                out.extend(aliases.iter().cloned());
            }
            if !out.contains(&p) {
                out.push(p);
            }
        }
        out
    }

    fn rerender(
        &self,
        changed: Vec<PathBuf>,
        specs: Vec<TargetSpec>,
        affected: BTreeSet<String>,
    ) -> ChangeSummary {
        let start = Instant::now();
        let to_render: Vec<TargetSpec> = specs
            .iter()
            .filter(|s| affected.contains(&s.name))
            .cloned()
            .collect();
        let report = self.inv.render_many(&to_render).unwrap_or_default();
        let current: BTreeSet<&String> = specs.iter().map(|s| &s.name).collect();

        let mut inner = self.inner.write();
        // Targets that disappeared or are being re-rendered leave the index.
        let gone: Vec<String> = inner
            .targets
            .keys()
            .filter(|n| !current.contains(n) || affected.contains(*n))
            .cloned()
            .collect();
        for name in gone {
            if let Some(old) = inner.targets.remove(&name) {
                for f in old.files.iter().chain(old.probes.iter()) {
                    if let Some(set) = inner.index.get_mut(f) {
                        set.remove(&name);
                    }
                }
            }
        }
        inner
            .errors
            .retain(|n, _| current.contains(n) && !affected.contains(n));
        inner.specs = specs;

        let rerendered: Vec<String> = to_render.iter().map(|s| s.name.clone()).collect();
        for (name, t) in report.targets {
            for f in t.files.iter().chain(t.probes.iter()) {
                inner
                    .index
                    .entry(f.clone())
                    .or_default()
                    .insert(name.clone());
            }
            for f in &t.files {
                if let Ok(real) = f.canonicalize()
                    && &real != f
                {
                    inner.aliases.entry(real).or_default().insert(f.clone());
                }
            }
            inner.targets.insert(name, Arc::new(t));
        }
        let errors: Vec<Diagnostic> = report
            .errors
            .into_iter()
            .map(|e| e.into_diagnostic())
            .collect();
        for e in &errors {
            if let Some(t) = &e.target {
                inner.errors.insert(t.clone(), e.clone());
            }
        }
        inner.index.retain(|_, set| !set.is_empty());

        inner.generation += 1;
        let summary = ChangeSummary {
            generation: inner.generation,
            at: now_ms(),
            changed_files: changed,
            rerendered,
            duration_ms: start.elapsed().as_millis() as u64,
            errors,
        };
        inner.history.push_back(summary.clone());
        while inner.history.len() > 200 {
            inner.history.pop_front();
        }
        drop(inner);
        let _guard = self.changed_lock.lock();
        self.changed.notify_all();
        summary
    }

    /// Block until the generation exceeds `since` or `timeout` passes.
    pub fn wait_for(&self, since: u64, timeout: Duration) -> (u64, bool, Vec<ChangeSummary>) {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let inner = self.inner.read();
                if inner.generation > since {
                    let changes = inner
                        .history
                        .iter()
                        .filter(|c| c.generation > since)
                        .cloned()
                        .collect();
                    return (inner.generation, false, changes);
                }
            }
            let now = Instant::now();
            if now >= deadline {
                return (self.inner.read().generation, true, Vec::new());
            }
            let mut guard = self.changed_lock.lock();
            self.changed.wait_for(&mut guard, deadline - now);
        }
    }

    pub fn is_inventory_file(path: &Path) -> bool {
        matches!(
            path.extension().and_then(|e| e.to_str()),
            Some("yml" | "yaml")
        )
    }
}

//! Resolvers written in Python, as kapitan's omegaconf backend allowed.
//!
//! The reference imports `<inventory-path>/resolvers.py` (falling back to
//! `system/omegaconf/resolvers/resolvers.py`), calls `pass_resolvers()` and
//! registers every function of the returned dict with OmegaConf, replacing
//! same-named built-ins. Here the same file runs in a pool of Python workers
//! (`runner/resolver_runner.py`); each function becomes a resolver that
//! serialises its arguments as JSON, answers `_root_` / `_parent_` lookups
//! from the evaluator while the function runs, and takes the result back.
//!
//! What the file defines is cached per content digest, so an unchanged file
//! costs no Python start-up until one of its resolvers is actually called.
//! Python wins over native resolvers of the same name unless
//! `prefer_native` is set (the native `contrib` set is a port of one such
//! file, and a port cannot follow the file's later edits).

use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::{Condvar, Mutex};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

use super::{ArgKind, Ctx, Registry, ResolverError, ResolverFn, ResolverResult};
use crate::dotkapitan::PythonResolverSettings;
use crate::path::Key;
use crate::python::{
    DEFAULT_REQUEST_TIMEOUT, PythonCmd, Worker, WorkerError, cache_dir, materialize_script,
    script_digest,
};
use crate::value::Value;

pub const RUNNER_SOURCE: &str = include_str!("../../runner/resolver_runner.py");

thread_local! {
    /// Workers this thread holds while a Python resolver runs. A resolver's
    /// `_root_` lookups evaluate interpolations that may call Python again;
    /// such a nested call must never wait for the pool, or every render
    /// thread ends up holding one worker while waiting for another.
    static HELD: Cell<usize> = const { Cell::new(0) };
}

/// How long `acquire` waits for a busy pool before giving up.
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
pub struct PythonConfig {
    /// The `resolvers.py` to import.
    pub file: PathBuf,
    /// Added to `sys.path` (the repository root, where `.kapitan` lives).
    pub cwd: PathBuf,
    pub python: PythonCmd,
    /// Keep native resolvers when the file defines the same name.
    pub prefer_native: bool,
    /// Upper bound on concurrently running worker processes.
    pub max_workers: usize,
    /// The deadline of each worker request (`compile.python-timeout`).
    pub timeout: Duration,
}

impl PythonConfig {
    /// The reference's discovery (`<inventory-path>/resolvers.py`, then
    /// `<cwd>/system/omegaconf/resolvers/resolvers.py`) with the `.kapitan`
    /// `inventory.python-resolvers` settings applied. `None` when disabled
    /// or when no file exists and none was configured.
    pub fn discover(
        inventory_path: &Path,
        cwd: &Path,
        settings: &PythonResolverSettings,
    ) -> Option<PythonConfig> {
        if settings.enabled == Some(false) {
            return None;
        }
        let file = match &settings.file {
            Some(f) if f.is_absolute() => f.clone(),
            Some(f) => cwd.join(f),
            None => [
                inventory_path.join("resolvers.py"),
                cwd.join("system/omegaconf/resolvers/resolvers.py"),
            ]
            .into_iter()
            .find(|p| p.is_file())?,
        };
        let max_workers = settings.workers.unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(1)
                .clamp(1, 8)
        });
        Some(PythonConfig {
            file: file.canonicalize().unwrap_or(file),
            cwd: cwd.to_path_buf(),
            python: PythonCmd::preferred(settings.python.as_deref()),
            prefer_native: settings.prefer_native.unwrap_or(false),
            max_workers: max_workers.max(1),
            timeout: DEFAULT_REQUEST_TIMEOUT,
        })
    }
}

/// What importing the file produced.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Loaded {
    /// Resolver name → the special arguments it declares (`_root_`, ...).
    pub resolvers: BTreeMap<String, Vec<String>>,
    /// Project modules the import loaded (not the standard library or
    /// installed packages); a change to one of them needs a fresh import.
    pub modules: Vec<PathBuf>,
    /// Version of the omegaconf package in the worker, `None` when the
    /// worker's stand-in module is used.
    pub omegaconf: Option<String>,
    pub python: String,
}

struct Pool {
    idle: Vec<Worker>,
    live: usize,
}

/// A pool of workers that imported one `resolvers.py`.
pub struct PythonResolvers {
    pub cfg: PythonConfig,
    pool: Mutex<Pool>,
    available: Condvar,
    loaded: OnceLock<Result<Loaded, String>>,
}

impl PythonResolvers {
    pub fn new(cfg: PythonConfig) -> Arc<Self> {
        Arc::new(PythonResolvers {
            cfg,
            pool: Mutex::new(Pool {
                idle: Vec::new(),
                live: 0,
            }),
            available: Condvar::new(),
            loaded: OnceLock::new(),
        })
    }

    /// Register the file's functions in `registry` and record the files the
    /// registry now depends on. Returns what the file defines.
    pub fn install(this: &Arc<Self>, registry: &mut Registry) -> Result<Loaded, String> {
        let loaded = this.load()?.clone();
        registry.add_source(this.cfg.file.clone());
        for m in &loaded.modules {
            registry.add_source(m.clone());
        }
        let mut kept_native = 0;
        for name in loaded.resolvers.keys() {
            if registry.contains(name) {
                if this.cfg.prefer_native {
                    tracing::debug!(resolver = %name, "native resolver kept over the Python one");
                    kept_native += 1;
                    continue;
                }
                tracing::debug!(resolver = %name, "Python resolver replaces the native one");
            }
            registry.register_arc(name, Self::resolver(this, name));
        }
        let mut description = format!(
            "{} Python resolvers from {} via {}",
            loaded.resolvers.len() - kept_native,
            this.cfg.file.display(),
            this.cfg.python.description
        );
        if kept_native > 0 {
            description.push_str(&format!(" ({kept_native} kept native)"));
        }
        registry.set_description(description);
        Ok(loaded)
    }

    /// What the file defines: from the cache when its content is known,
    /// else by importing it in the first worker. The outcome is remembered,
    /// so a broken file reports once.
    pub fn load(&self) -> Result<&Loaded, String> {
        self.loaded
            .get_or_init(|| {
                if let Some(loaded) = self.cached() {
                    tracing::debug!(file = %self.cfg.file.display(), "Python resolvers known from cache");
                    return Ok(loaded);
                }
                let worker = self.spawn()?;
                let loaded = Self::parse_info(&worker.info);
                tracing::info!(
                    file = %self.cfg.file.display(),
                    resolvers = loaded.resolvers.len(),
                    python = %loaded.python,
                    omegaconf = loaded.omegaconf.as_deref().unwrap_or("stand-in"),
                    "imported Python resolvers"
                );
                self.remember(&loaded);
                let mut pool = self.pool.lock();
                pool.live += 1;
                pool.idle.push(worker);
                Ok(loaded)
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// Cache entry: file content, interpreter and worker script.
    fn cache_path(&self) -> Option<PathBuf> {
        let content = std::fs::read(&self.cfg.file).ok()?;
        let mut h = blake3::Hasher::new();
        h.update(&content);
        h.update(self.cfg.python.cache_key().as_bytes());
        h.update(script_digest(RUNNER_SOURCE).as_bytes());
        let key = h.finalize().to_hex();
        Some(
            cache_dir()
                .join("resolvers")
                .join(format!("{}.json", &key[..32])),
        )
    }

    fn cached(&self) -> Option<Loaded> {
        let text = std::fs::read_to_string(self.cache_path()?).ok()?;
        let loaded: Loaded = serde_json::from_str(&text).ok()?;
        // A module that vanished means the import would go differently.
        loaded.modules.iter().all(|m| m.is_file()).then_some(loaded)
    }

    fn remember(&self, loaded: &Loaded) {
        let Some(path) = self.cache_path() else {
            return;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, serde_json::to_string_pretty(loaded).unwrap());
    }

    fn resolver(this: &Arc<Self>, name: &str) -> Arc<ResolverFn> {
        let me = this.clone();
        let name = name.to_string();
        Arc::new(move |ctx: &mut Ctx, args: &[Value]| me.call(&name, ctx, args))
    }

    fn parse_info(info: &Json) -> Loaded {
        let strings = |v: Option<&Json>| -> Vec<String> {
            v.and_then(Json::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Json::as_str)
                        .map(str::to_string)
                        .collect()
                })
                .unwrap_or_default()
        };
        let resolvers = info
            .get("resolvers")
            .and_then(Json::as_object)
            .map(|m| {
                m.iter()
                    .map(|(name, spec)| (name.clone(), strings(spec.get("wants"))))
                    .collect()
            })
            .unwrap_or_default();
        Loaded {
            resolvers,
            modules: strings(info.get("modules"))
                .into_iter()
                .map(PathBuf::from)
                .collect(),
            omegaconf: info
                .get("omegaconf")
                .and_then(Json::as_str)
                .map(str::to_string),
            python: info
                .get("python")
                .and_then(Json::as_str)
                .unwrap_or("unknown")
                .to_string(),
        }
    }

    fn spawn(&self) -> Result<Worker, String> {
        let script = materialize_script("resolver_runner.py", RUNNER_SOURCE)
            .map_err(|e| format!("cannot write the Python resolver worker script: {e}"))?;
        let init = json!({ "file": self.cfg.file, "cwd": self.cfg.cwd });
        Worker::spawn(&self.cfg.python, &script, init, self.cfg.timeout).map_err(|e| match e {
            WorkerError::Failed { error, traceback } => {
                if let Some(tb) = traceback {
                    tracing::debug!("{tb}");
                }
                format!(
                    "cannot load Python resolvers from {}: {error}",
                    self.cfg.file.display()
                )
            }
            other => format!(
                "cannot start the Python resolver worker ({}): {other}",
                self.cfg.python.description
            ),
        })
    }

    fn acquire(&self) -> Result<Worker, String> {
        let nested = HELD.with(|h| h.get() > 0);
        let deadline = Instant::now() + ACQUIRE_TIMEOUT;
        let mut pool = self.pool.lock();
        loop {
            if let Some(w) = pool.idle.pop() {
                return Ok(w);
            }
            // A nested call spawns past `max_workers`: the worker this thread
            // already holds is blocked on this very call, so waiting for it
            // (or for another thread in the same position) would never end.
            if nested || pool.live < self.cfg.max_workers {
                pool.live += 1;
                drop(pool);
                return match self.spawn() {
                    Ok(w) => Ok(w),
                    Err(e) => {
                        self.discard();
                        Err(e)
                    }
                };
            }
            if self.available.wait_until(&mut pool, deadline).timed_out() {
                return Err(format!(
                    "no Python resolver worker became free in {}s ({} busy, `workers: {}`)",
                    ACQUIRE_TIMEOUT.as_secs(),
                    pool.live,
                    self.cfg.max_workers
                ));
            }
        }
    }

    fn release(&self, worker: Worker) {
        let mut pool = self.pool.lock();
        if pool.live > 2 * self.cfg.max_workers {
            // Spawned for a nested call while the pool was already over its
            // size; retire it rather than keep an idle process around.
            pool.live -= 1;
            drop(pool);
            drop(worker);
        } else {
            pool.idle.push(worker);
            drop(pool);
        }
        self.available.notify_one();
    }

    /// A worker is gone (it died, or could not be started).
    fn discard(&self) {
        self.pool.lock().live -= 1;
        self.available.notify_one();
    }

    fn call(&self, name: &str, ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
        let req = json!({
            "op": "call",
            "name": name,
            "args": args.iter().map(Value::to_json).collect::<Vec<_>>(),
            "arg_kinds": ctx.arg_kinds.iter().map(kind_name).collect::<Vec<_>>(),
            "node": {
                "key": key_json(ctx.key()),
                "full_key": ctx.full_key(),
                "parent_key": key_json(ctx.parent_key()),
                "parent_full_key": ctx.at.parent().map(|p| p.to_omegaconf()),
            },
        });
        let mut attempts = 0;
        loop {
            attempts += 1;
            let mut worker = self.acquire().map_err(ResolverError::Message)?;
            // The first evaluator error met while answering lookups; it is
            // the real failure when the Python side then gives up.
            let mut inner: Option<ResolverError> = None;
            HELD.with(|h| h.set(h.get() + 1));
            let outcome = worker.call_with(req.clone(), |r| answer(ctx, r, &mut inner));
            HELD.with(|h| h.set(h.get() - 1));
            match outcome {
                Ok(msg) => {
                    self.release(worker);
                    return Ok(Value::from(msg.get("value").cloned().unwrap_or(Json::Null)));
                }
                Err(WorkerError::Failed { error, traceback }) => {
                    self.release(worker);
                    if let Some(e) = inner {
                        return Err(e);
                    }
                    if let Some(tb) = traceback {
                        tracing::debug!(resolver = name, "{tb}");
                    }
                    return Err(ResolverError::Message(error));
                }
                Err(e) => {
                    drop(worker);
                    self.discard();
                    // A retry would only wait out the deadline again.
                    if let WorkerError::Timeout(_) = e {
                        return Err(e.to_string().into());
                    }
                    if attempts >= 2 {
                        return Err(format!("Python resolver worker died: {e}").into());
                    }
                    tracing::warn!(
                        resolver = name,
                        "Python resolver worker died ({e}), retrying"
                    );
                }
            }
        }
    }
}

/// Answer one lookup the worker asks for while a resolver runs.
fn answer(ctx: &mut Ctx, req: &Json, inner: &mut Option<ResolverError>) -> Json {
    match req.get("op").and_then(Json::as_str) {
        Some("select") => {
            let key = req.get("key").and_then(Json::as_str).unwrap_or("");
            let relative = req.get("relative").and_then(Json::as_bool).unwrap_or(false);
            // Relative keys use OmegaConf's syntax from the node's parent:
            // `.` is the parent itself, `.x` a sibling. The absolute empty
            // key is the root.
            let full = if relative && !key.starts_with('.') {
                format!(".{key}")
            } else {
                key.to_string()
            };
            match ctx.select(&full) {
                Ok(Some(v)) => json!({ "ok": true, "found": true, "value": v.to_json() }),
                Ok(None) => json!({ "ok": true, "found": false }),
                Err(e) => {
                    let message = match &e {
                        ResolverError::Message(m) => m.clone(),
                        ResolverError::Inner(err) => err.to_string(),
                    };
                    if inner.is_none() {
                        *inner = Some(e);
                    }
                    json!({ "ok": false, "error": message })
                }
            }
        }
        other => json!({ "ok": false, "error": format!("unsupported host request {other:?}") }),
    }
}

fn kind_name(k: &ArgKind) -> &'static str {
    match k {
        ArgKind::Literal => "literal",
        ArgKind::Node => "node",
        ArgKind::Computed => "computed",
    }
}

fn key_json(k: Option<Key>) -> Json {
    match k {
        Some(Key::Str(s)) => Json::String(s),
        Some(Key::Index(i)) => json!(i),
        None => Json::Null,
    }
}

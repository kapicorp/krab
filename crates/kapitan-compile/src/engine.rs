//! The incremental compiler: decide which targets are stale, compile them on
//! a pool of Python workers, install the results and record what they read.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;
use rayon::prelude::*;
use serde::Serialize;
use serde_json::{Value, json};

use crate::digest::{Digests, digest_str};
use crate::fetch::{self, FetchOptions, FetchOutcome};
use crate::inputs::Reads;
use crate::inputs::kadet::kadet_runner_digest;
use crate::manifest::{ItemRecord, MANIFEST_FILE, MANIFEST_VERSION, Manifest, TargetRecord};
use crate::native::{ItemContext, NativeCompiler, NativeOptions};
use crate::plan::TargetPlan;
use crate::python::{PythonCmd, materialize_runner, runner_digest};
use crate::worker::{Worker, WorkerError};

/// Name prefix of the per-run staging directory inside `compiled/`. A full
/// run's cleanup removes any left behind by a crashed process.
const STAGING_PREFIX: &str = ".kapitan2-staging-";

/// How targets are compiled.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Backend {
    /// Native input types; Python only evaluates kadet components.
    Native,
    /// kapitan's Python input types inside a worker process (reference behaviour).
    Python,
}

#[derive(Clone, Debug)]
pub struct CompileOptions {
    /// Directory holding `.kapitan` (kapitan's working directory).
    pub repo_root: PathBuf,
    /// `compiled/` is created under this directory.
    pub output_path: PathBuf,
    pub search_paths: Vec<PathBuf>,
    /// Extra flags for kapitan's `compile` argument parser (e.g. `--reveal`).
    pub flags: Vec<String>,
    pub python: PythonCmd,
    pub parallelism: usize,
    /// Recompile everything regardless of the manifest.
    pub force: bool,
    /// Only report what would be compiled (and fetched).
    pub dry_run: bool,
    /// Fetch every `parameters.kapitan.dependencies` item whose output is
    /// missing (`--fetch`); without it only items with `force_fetch: true`.
    pub fetch: bool,
    /// Fetch every dependency and overwrite what exists (`--force-fetch`).
    pub force_fetch: bool,
    pub backend: Backend,
    /// Settings for the native backend.
    pub native: NativeOptions,
}

impl CompileOptions {
    pub fn compiled_dir(&self) -> PathBuf {
        self.output_path.join("compiled")
    }

    pub fn manifest_path(&self) -> PathBuf {
        self.compiled_dir().join(MANIFEST_FILE)
    }

    fn config_digest(&self) -> String {
        let repr = json!({
            "search_paths": self.search_paths,
            "flags": self.flags,
            "output_path": self.output_path,
            "backend": format!("{:?}", self.backend),
            "native": format!("{:?}", self.native),
        });
        digest_str(&repr.to_string())
    }

    fn engine_identity(&self, versions: &str) -> String {
        match self.backend {
            Backend::Native => format!(
                "kapitan {} native {} {versions}",
                env!("CARGO_PKG_VERSION"),
                kadet_runner_digest()
            ),
            Backend::Python => format!(
                "kapitan {} runner {} {versions}",
                env!("CARGO_PKG_VERSION"),
                runner_digest()
            ),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Status {
    UpToDate,
    WouldCompile,
    Compiled {
        ms: u64,
    },
    Failed {
        error: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        traceback: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize)]
pub struct Outcome {
    pub target: String,
    #[serde(flatten)]
    pub status: Status,
    /// Why the target was (or would be) compiled.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// kadet items whose previous output was reused instead of evaluated.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub reused_items: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub outcomes: Vec<Outcome>,
    /// Dependencies fetched (or that would be) before compiling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fetched: Vec<FetchOutcome>,
    pub removed: Vec<PathBuf>,
    pub elapsed_ms: u64,
    pub manifest: PathBuf,
    pub engine: String,
}

impl Report {
    pub fn compiled(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| matches!(o.status, Status::Compiled { .. }))
            .count()
    }
    pub fn up_to_date(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| matches!(o.status, Status::UpToDate))
            .count()
    }
    pub fn failed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|o| matches!(o.status, Status::Failed { .. }))
            .count()
    }
}

pub enum Event<'a> {
    Planned {
        stale: usize,
        total: usize,
        workers: usize,
    },
    Started(&'a str),
    Finished(&'a Outcome),
    Removed(&'a Path),
    /// A dependency was fetched, skipped or failed (before `Planned`).
    Fetched(&'a FetchOutcome),
}

/// Where rendered target documents come from (the inventory server or a
/// local render). Documents are only fetched for targets that will compile.
pub trait DocSource: Sync {
    /// Every target with the digest of its rendered document.
    fn digests(&self) -> Result<BTreeMap<String, String>, String>;
    /// Rendered documents of the named targets.
    fn docs(&self, names: &[String]) -> Result<BTreeMap<String, Value>, String>;
    /// Rendered documents of every target (workers need the global inventory).
    fn all_docs(&self) -> Result<BTreeMap<String, Value>, String>;
    /// `parameters.kapitan.dependencies` of the named targets; targets
    /// without any are left out.
    fn dependencies(&self, names: &[String]) -> Result<BTreeMap<String, Value>, String> {
        Ok(declared_dependencies(self.docs(names)?))
    }
    /// On-demand access to documents for generators and templates.
    fn provider(&self) -> crate::docs::SharedDocs;
}

/// Which targets to consider.
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub targets: Vec<String>,
    pub labels: Vec<(String, String)>,
}

impl Selection {
    pub fn is_everything(&self) -> bool {
        self.targets.is_empty() && self.labels.is_empty()
    }
}

pub fn compile(
    source: &dyn DocSource,
    selection: &Selection,
    opts: &CompileOptions,
    on_event: &(dyn Fn(Event) + Sync),
) -> Result<Report, String> {
    let start = Instant::now();
    let manifest_path = opts.manifest_path();
    let mut manifest = Manifest::load(&manifest_path);
    let versions = opts
        .python
        .versions()
        .unwrap_or_else(|e| format!("unknown ({e})"));
    let engine = opts.engine_identity(&versions);
    let config_digest = opts.config_digest();
    let digests = Digests::new();
    let compiled_dir = opts.compiled_dir();

    let all_digests = source.digests()?;
    let everything_digest =
        digest_str(&all_digests.values().cloned().collect::<Vec<_>>().join("\n"));
    let target_paths: BTreeSet<String> = all_digests.keys().map(|n| n.replace('.', "/")).collect();

    // Candidates: by name, by label (needs the documents), or everything.
    let mut candidates: Vec<String> = all_digests.keys().cloned().collect();
    if !selection.targets.is_empty() {
        for t in &selection.targets {
            if !all_digests.contains_key(t) {
                return Err(format!(
                    "target `{t}` not found; list targets with `kapitan inventory targets`"
                ));
            }
        }
        candidates.retain(|n| selection.targets.contains(n));
    }
    if !selection.labels.is_empty() {
        let label_docs = source.docs(&candidates)?;
        candidates.retain(|n| {
            label_docs.get(n).is_some_and(|d| {
                TargetPlan::new(n, d.clone(), &opts.search_paths).matches_labels(&selection.labels)
            })
        });
    }
    if candidates.is_empty() {
        return Err("no matching targets".into());
    }

    // 0. Dependencies, before staleness is decided: the fetched files are inputs.
    let fetched = fetch_dependencies(source, &candidates, opts, on_event)?;

    // 1. Staleness, in parallel, from digests and the manifest alone.
    let decisions: Vec<(String, Option<String>)> = candidates
        .par_iter()
        .map(|name| {
            let reason = if opts.force {
                Some("forced".to_string())
            } else {
                why_stale(
                    name,
                    &all_digests[name],
                    manifest.targets.get(name),
                    &manifest.files,
                    &engine,
                    &config_digest,
                    &digests,
                    &all_digests,
                    &everything_digest,
                    opts,
                    &compiled_dir,
                    &target_paths,
                )
            };
            (name.clone(), reason)
        })
        .collect();

    let mut outcomes: Vec<Outcome> = Vec::new();
    let mut stale_names: Vec<(String, String)> = Vec::new();
    for (name, reason) in decisions {
        match reason {
            None => outcomes.push(Outcome {
                target: name,
                status: Status::UpToDate,
                reason: "up to date".into(),
                warnings: vec![],
                reused_items: 0,
            }),
            Some(r) => stale_names.push((name, r)),
        }
    }
    stale_names.sort();
    let total = outcomes.len() + stale_names.len();
    let workers = opts.parallelism.clamp(1, stale_names.len().max(1));
    on_event(Event::Planned {
        stale: stale_names.len(),
        total,
        workers,
    });

    if opts.dry_run {
        for (name, reason) in stale_names {
            outcomes.push(Outcome {
                target: name,
                status: Status::WouldCompile,
                reason,
                warnings: vec![],
                reused_items: 0,
            });
        }
        outcomes.sort_by(|a, b| a.target.cmp(&b.target));
        return Ok(Report {
            outcomes,
            fetched,
            removed: vec![],
            elapsed_ms: start.elapsed().as_millis() as u64,
            manifest: manifest_path,
            engine,
        });
    }

    let mut removed = Vec::new();
    if !stale_names.is_empty() {
        // 2. Documents (all of them: workers serve the global inventory) and plans.
        // With a server, only the stale targets' documents are fetched;
        // generators and templates pull other targets from the server on demand.
        let provider = source.provider();
        let socket = provider
            .socket()
            .filter(|_| opts.backend == Backend::Native);
        let all_docs: BTreeMap<String, Value> = if socket.is_some() {
            source.docs(
                &stale_names
                    .iter()
                    .map(|(n, _)| n.clone())
                    .collect::<Vec<_>>(),
            )?
        } else {
            source.all_docs()?
        };
        let stale: Vec<(TargetPlan, String)> = stale_names
            .into_iter()
            .filter_map(|(name, reason)| {
                all_docs.get(&name).map(|d| {
                    (
                        TargetPlan::new(&name, d.clone(), &opts.search_paths),
                        reason,
                    )
                })
            })
            .collect();

        // 3. Workers. Outputs are staged inside `compiled/` so the install is
        // a rename on the same filesystem, never a copy (the system temp dir
        // is usually another mount, or tmpfs).
        let temp_root = compiled_dir.join(format!(
            "{STAGING_PREFIX}{}-{}",
            std::process::id(),
            start.elapsed().as_nanos()
        ));
        std::fs::create_dir_all(temp_root.join("compiled")).map_err(|e| e.to_string())?;
        let inventory_file = temp_root.join("inventory.json");
        if socket.is_none() {
            std::fs::write(&inventory_file, serde_json::to_vec(&all_docs).unwrap())
                .map_err(|e| e.to_string())?;
        }
        let script = materialize_runner().map_err(|e| e.to_string())?;
        let init = json!({
            "cwd": opts.repo_root,
            "inventory_file": inventory_file,
            "temp_root": temp_root,
            "flags": opts.flags,
        });
        let native = match opts.backend {
            Backend::Native => Some(
                NativeCompiler::new(
                    opts.native.clone(),
                    opts.python.clone(),
                    socket.as_deref(),
                    &inventory_file,
                    &opts.flags,
                    provider.clone(),
                )
                .map_err(|e| format!("cannot set up the native compiler: {e}"))?,
            ),
            Backend::Python => None,
        };

        manifest.engine = engine.clone();
        manifest.version = MANIFEST_VERSION;
        // Item reuse checks files against what the last compile saw, like `why_stale`.
        let files_before = manifest.files.clone();
        let manifest = Arc::new(Mutex::new(manifest));
        let queue: Arc<Mutex<VecDeque<(TargetPlan, String)>>> =
            Arc::new(Mutex::new(stale.into_iter().collect()));
        let results: Arc<Mutex<Vec<Outcome>>> = Arc::new(Mutex::new(Vec::new()));
        let ctx = Ctx {
            opts,
            engine: &engine,
            native: native.as_ref(),
            compiled_dir: &compiled_dir,
            temp_root: &temp_root,
            all_digests: &all_digests,
            everything_digest: &everything_digest,
            config_digest: &config_digest,
            target_paths: &target_paths,
            manifest_path: &manifest_path,
            files_before: &files_before,
            digests: &digests,
        };

        std::thread::scope(|scope| {
            for _ in 0..workers {
                let queue = queue.clone();
                let results = results.clone();
                let manifest = manifest.clone();
                let init = init.clone();
                let script = &script;
                let ctx = &ctx;
                scope.spawn(move || {
                    let mut worker: Option<Worker> = None;
                    loop {
                        let Some((plan, reason)) = queue.lock().pop_front() else {
                            break;
                        };
                        on_event(Event::Started(&plan.name));
                        let outcome =
                            run_one(&mut worker, &plan, reason, script, &init, ctx, &manifest);
                        on_event(Event::Finished(&outcome));
                        results.lock().push(outcome);
                    }
                });
            }
        });
        let _ = std::fs::remove_dir_all(&temp_root);
        outcomes.extend(results.lock().drain(..));
        let manifest = manifest.lock();
        manifest
            .save(&manifest_path)
            .map_err(|e| format!("cannot save manifest {}: {e}", manifest_path.display()))?;
    } else if manifest.engine != engine || !manifest_path.exists() {
        manifest.engine = engine.clone();
        manifest.version = MANIFEST_VERSION;
        manifest
            .save(&manifest_path)
            .map_err(|e| format!("cannot save manifest {}: {e}", manifest_path.display()))?;
    }

    // 4. Full runs drop output of targets that no longer exist.
    if selection.is_everything() && compiled_dir.is_dir() {
        cleanup(&compiled_dir, &compiled_dir, &target_paths, &mut removed);
        for r in &removed {
            on_event(Event::Removed(r));
        }
        let mut manifest = Manifest::load(&manifest_path);
        manifest
            .targets
            .retain(|name, _| all_digests.contains_key(name));
        manifest.prune_files();
        manifest
            .save(&manifest_path)
            .map_err(|e| format!("cannot save manifest {}: {e}", manifest_path.display()))?;
    }

    outcomes.sort_by(|a, b| a.target.cmp(&b.target));
    Ok(Report {
        outcomes,
        fetched,
        removed,
        elapsed_ms: start.elapsed().as_millis() as u64,
        manifest: manifest_path,
        engine,
    })
}

/// `parameters.kapitan.dependencies` of each document that has one.
pub fn declared_dependencies(docs: BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    docs.into_iter()
        .filter_map(|(n, d)| {
            d.pointer("/parameters/kapitan/dependencies")
                .filter(|v| !v.is_null())
                .cloned()
                .map(|v| (n, v))
        })
        .collect()
}

/// Fetch the dependencies the candidate targets declare. Every declared
/// item is considered with `--fetch`/`--force-fetch`; otherwise only items
/// marked `force_fetch: true`. A failed fetch aborts the compile.
fn fetch_dependencies(
    source: &dyn DocSource,
    candidates: &[String],
    opts: &CompileOptions,
    on_event: &(dyn Fn(Event) + Sync),
) -> Result<Vec<FetchOutcome>, String> {
    let declared = source.dependencies(candidates)?;
    let mut deps = Vec::new();
    for (name, value) in &declared {
        deps.extend(fetch::dependencies(name, value, &opts.output_path)?);
    }
    if deps.is_empty() {
        return Ok(vec![]);
    }
    let fetched = fetch::fetch(
        deps,
        &FetchOptions {
            repo_root: &opts.repo_root,
            fetch_all: opts.fetch || opts.force_fetch,
            force: opts.force_fetch,
            dry_run: opts.dry_run,
            parallelism: opts.parallelism,
            cache_dir: crate::python::cache_dir(),
        },
    );
    for f in &fetched {
        on_event(Event::Fetched(f));
    }
    let failed = fetched.iter().filter(|f| f.failed()).count();
    if failed > 0 {
        return Err(format!(
            "{failed} dependenc{} failed to fetch",
            if failed == 1 { "y" } else { "ies" }
        ));
    }
    Ok(fetched)
}

struct Ctx<'a> {
    opts: &'a CompileOptions,
    engine: &'a str,
    native: Option<&'a NativeCompiler>,
    compiled_dir: &'a Path,
    temp_root: &'a Path,
    all_digests: &'a BTreeMap<String, String>,
    everything_digest: &'a str,
    config_digest: &'a str,
    target_paths: &'a BTreeSet<String>,
    manifest_path: &'a Path,
    files_before: &'a BTreeMap<String, String>,
    digests: &'a Digests,
}

fn run_one(
    worker: &mut Option<Worker>,
    plan: &TargetPlan,
    reason: String,
    script: &Path,
    init: &Value,
    ctx: &Ctx,
    manifest: &Mutex<Manifest>,
) -> Outcome {
    let started = Instant::now();
    if let Some(native) = ctx.native {
        let temp_dir = ctx.temp_root.join(&plan.name);
        let _ = std::fs::remove_dir_all(&temp_dir);
        let temp_target = temp_dir.join("compiled").join(&plan.target_path);
        // Items of the last compile can be reused only if the same compiler
        // with the same settings produced them, and never under --force.
        let previous: Vec<ItemRecord> = manifest
            .lock()
            .targets
            .get(&plan.name)
            .filter(|r| {
                !ctx.opts.force && r.engine == ctx.engine && r.config_digest == ctx.config_digest
            })
            .map(|r| r.items.clone())
            .unwrap_or_default();
        let items = ItemContext {
            previous: &previous,
            files: ctx.files_before,
            all_digests: ctx.all_digests,
            everything_digest: ctx.everything_digest,
            compiled_dir: ctx.compiled_dir,
            digests: ctx.digests,
            repo_root: &ctx.opts.repo_root,
        };
        let outcome = match native.compile_target(plan, &temp_dir, &items) {
            Ok(out) => {
                match install_and_record(
                    plan,
                    out.reads,
                    out.items,
                    &temp_target,
                    ctx,
                    manifest,
                    started,
                ) {
                    Ok(()) => Outcome {
                        target: plan.name.clone(),
                        status: Status::Compiled {
                            ms: started.elapsed().as_millis() as u64,
                        },
                        reason,
                        warnings: out.warnings,
                        reused_items: out.reused,
                    },
                    Err(e) => Outcome {
                        target: plan.name.clone(),
                        status: Status::Failed {
                            error: e,
                            traceback: None,
                        },
                        reason,
                        warnings: out.warnings,
                        reused_items: 0,
                    },
                }
            }
            Err(e) => Outcome {
                target: plan.name.clone(),
                status: Status::Failed {
                    error: e,
                    traceback: None,
                },
                reason,
                warnings: vec![],
                reused_items: 0,
            },
        };
        let _ = std::fs::remove_dir_all(&temp_dir);
        return outcome;
    }
    let mut attempts = 0;
    loop {
        attempts += 1;
        if worker.is_none() {
            match Worker::spawn(&ctx.opts.python, script, init.clone()) {
                Ok(w) => *worker = Some(w),
                Err(e) => {
                    return Outcome {
                        target: plan.name.clone(),
                        status: Status::Failed {
                            error: format!("cannot start compile worker: {e}"),
                            traceback: None,
                        },
                        reason,
                        warnings: vec![],
                        reused_items: 0,
                    };
                }
            }
        }
        // Each target compiles into its own temporary tree so nested targets
        // (`a.b` and `a.b.c`) never see each other's half-written output.
        let temp_dir = ctx.temp_root.join(&plan.name);
        let _ = std::fs::remove_dir_all(&temp_dir);
        let temp_target = temp_dir.join("compiled").join(&plan.target_path);
        let req = json!({
            "op": "compile",
            "target": plan.name,
            "target_path": plan.target_path,
            "temp_dir": temp_dir,
            "compile_path": temp_dir.join("compiled"),
            "compile": plan.compile,
        });
        match worker.as_mut().unwrap().call(req) {
            Ok(resp) => {
                let warnings: Vec<String> = resp
                    .get("warnings")
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_string)
                            .collect()
                    })
                    .unwrap_or_default();
                let installed = install_and_record(
                    plan,
                    reads_from_response(&resp),
                    vec![],
                    &temp_target,
                    ctx,
                    manifest,
                    started,
                );
                let _ = std::fs::remove_dir_all(&temp_dir);
                match installed {
                    Ok(()) => {
                        return Outcome {
                            target: plan.name.clone(),
                            status: Status::Compiled {
                                ms: started.elapsed().as_millis() as u64,
                            },
                            reason,
                            warnings,
                            reused_items: 0,
                        };
                    }
                    Err(e) => {
                        return Outcome {
                            target: plan.name.clone(),
                            status: Status::Failed {
                                error: e,
                                traceback: None,
                            },
                            reason,
                            warnings,
                            reused_items: 0,
                        };
                    }
                }
            }
            Err(WorkerError::Failed { error, traceback }) => {
                return Outcome {
                    target: plan.name.clone(),
                    status: Status::Failed { error, traceback },
                    reason,
                    warnings: vec![],
                    reused_items: 0,
                };
            }
            Err(e) => {
                // The worker died; start a fresh one and retry once.
                *worker = None;
                if attempts >= 2 {
                    return Outcome {
                        target: plan.name.clone(),
                        status: Status::Failed {
                            error: e.to_string(),
                            traceback: None,
                        },
                        reason,
                        warnings: vec![],
                        reused_items: 0,
                    };
                }
            }
        }
    }
}

/// Dependency information reported by the Python worker.
fn reads_from_response(resp: &Value) -> Reads {
    let mut reads = Reads::default();
    for p in resp
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        reads.file(Path::new(p));
    }
    for p in resp
        .get("dirs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        reads.dir(Path::new(p));
    }
    reads.globals.extend(
        resp.get("globals")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_string),
    );
    reads
}

#[allow(clippy::too_many_arguments)]
fn install_and_record(
    plan: &TargetPlan,
    reads: Reads,
    items: Vec<ItemRecord>,
    temp_target: &Path,
    ctx: &Ctx,
    manifest: &Mutex<Manifest>,
    started: Instant,
) -> Result<(), String> {
    let final_dir = ctx.compiled_dir.join(&plan.target_path);
    let children = child_names(&plan.target_path, ctx.target_paths);
    install(temp_target, &final_dir, &children)
        .map_err(|e| format!("cannot install output: {e}"))?;
    // Files the items already fingerprinted while writing keep that
    // fingerprint at their installed path; only the rest are read back.
    let outputs = Digests::new();
    for item in &items {
        for (rel, fp) in &item.outputs {
            outputs.seed(ctx.compiled_dir.join(rel), fp.clone());
        }
    }
    let output_digest = outputs.tree(&final_dir, &children);

    let root = &ctx.opts.repo_root;
    let mut deps: BTreeMap<String, String> = BTreeMap::new();
    let mut record_dep = |p: &Path| {
        if let Ok(rel) = p.strip_prefix(root) {
            let rel = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            // A fresh fingerprint: the compile may have just written it (e.g. refs).
            deps.insert(rel, Digests::new().fingerprint(p));
        }
    };
    for p in reads.files.iter().chain(reads.dirs.iter()) {
        record_dep(p);
    }
    for p in &plan.probes {
        record_dep(p);
    }
    let mut globals = BTreeMap::new();
    if reads.globals.iter().any(|g| g == "*") {
        globals.insert("*".to_string(), ctx.everything_digest.to_string());
    } else {
        for g in &reads.globals {
            if g != &plan.name
                && let Some(d) = ctx.all_digests.get(g)
            {
                globals.insert(g.to_string(), d.clone());
            }
        }
    }
    let record = TargetRecord {
        target_path: plan.target_path.clone(),
        doc_digest: plan.doc_digest.clone(),
        config_digest: ctx.config_digest.to_string(),
        engine: ctx.engine.to_string(),
        deps: deps.keys().cloned().collect(),
        globals,
        output_digest,
        compiled_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        duration_ms: started.elapsed().as_millis() as u64,
        items,
    };
    let mut m = manifest.lock();
    m.files.extend(deps);
    m.targets.insert(plan.name.clone(), record);
    m.save(ctx.manifest_path)
        .map_err(|e| format!("cannot save manifest: {e}"))
}

#[allow(clippy::too_many_arguments)]
fn why_stale(
    name: &str,
    doc_digest: &str,
    record: Option<&TargetRecord>,
    manifest_files: &BTreeMap<String, String>,
    engine: &str,
    config_digest: &str,
    digests: &Digests,
    all_digests: &BTreeMap<String, String>,
    everything_digest: &str,
    opts: &CompileOptions,
    compiled_dir: &Path,
    target_paths: &BTreeSet<String>,
) -> Option<String> {
    let Some(record) = record else {
        return Some("never compiled".into());
    };
    if record.engine != engine {
        return Some("compiler changed".into());
    }
    if record.doc_digest != doc_digest {
        return Some("inventory changed".into());
    }
    if record.config_digest != config_digest {
        return Some("compile settings changed".into());
    }
    for rel in &record.deps {
        let fp = manifest_files.get(rel).map(String::as_str).unwrap_or("?");
        let now = digests.fingerprint(&opts.repo_root.join(rel));
        if now != fp {
            return Some(match (fp, now.as_str()) {
                ("-", _) => format!("{rel} appeared"),
                (_, "-") => format!("{rel} removed"),
                _ => format!("{rel} changed"),
            });
        }
    }
    for (name, d) in &record.globals {
        if name == "*" {
            if d != everything_digest {
                return Some(
                    "inventory of another target changed (this target reads the global inventory)"
                        .into(),
                );
            }
        } else if all_digests.get(name) != Some(d) {
            return Some(format!(
                "inventory of target {name} changed (read by this target)"
            ));
        }
    }
    let target_path = name.replace('.', "/");
    let final_dir = compiled_dir.join(&target_path);
    if !final_dir.is_dir() {
        return Some("compiled output missing".into());
    }
    if Digests::new().tree(&final_dir, &child_names(&target_path, target_paths))
        != record.output_digest
    {
        return Some("compiled output was modified".into());
    }
    None
}

/// Top-level entries under a target's directory that belong to nested targets.
fn child_names(target_path: &str, target_paths: &BTreeSet<String>) -> Vec<String> {
    let prefix = format!("{target_path}/");
    let mut names: Vec<String> = target_paths
        .iter()
        .filter_map(|p| p.strip_prefix(&prefix))
        .map(|rest| rest.split('/').next().unwrap_or("").to_string())
        .filter(|s| !s.is_empty())
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Replace the target's output with the freshly compiled one, keeping nested targets.
fn install(temp_target: &Path, final_dir: &Path, children: &[String]) -> std::io::Result<()> {
    std::fs::create_dir_all(final_dir)?;
    for entry in std::fs::read_dir(final_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if children.contains(&name) || name == MANIFEST_FILE || name.starts_with(STAGING_PREFIX) {
            continue;
        }
        let p = entry.path();
        if p.is_dir() {
            std::fs::remove_dir_all(&p)?
        } else {
            std::fs::remove_file(&p)?
        }
    }
    if temp_target.is_dir() {
        for entry in std::fs::read_dir(temp_target)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if children.contains(&name) {
                // A nested target's directory produced by this target's compile
                // (kapitan writes into `compiled/<path>`); merge instead of replace.
                merge_dirs(&entry.path(), &final_dir.join(&name))?;
                continue;
            }
            move_path(&entry.path(), &final_dir.join(&name))?;
        }
    }
    Ok(())
}

fn merge_dirs(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            merge_dirs(&entry.path(), &dest)?;
        } else {
            if dest.exists() {
                std::fs::remove_file(&dest)?;
            }
            move_path(&entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn move_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            move_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        std::fs::remove_dir_all(from)
    } else {
        std::fs::copy(from, to)?;
        std::fs::remove_file(from)
    }
}

/// Remove directories under `compiled/` that belong to no target.
fn cleanup(root: &Path, dir: &Path, target_paths: &BTreeSet<String>, removed: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let path = entry.path();
        let rel = path
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if path.is_dir() {
            if target_paths.contains(&rel) {
                continue;
            }
            let prefix = format!("{rel}/");
            if target_paths.iter().any(|t| t.starts_with(&prefix)) {
                cleanup(root, &path, target_paths, removed);
            } else if std::fs::remove_dir_all(&path).is_ok() {
                removed.push(path);
            }
        } else if rel != MANIFEST_FILE && std::fs::remove_file(&path).is_ok() {
            removed.push(path);
        }
    }
}

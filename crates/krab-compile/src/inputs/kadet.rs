//! `kadet`: evaluate a Python component through a small evaluator process
//! (`kadet_runner.py`) and get its output object back as JSON. Everything
//! else — formatting, refs, writing — is done natively.
//!
//! The evaluator ships with its own `kapitan` package (`runner/kapitan`):
//! the API components import (`inventory()`, `inventory_global()`,
//! `HelmChart`, `render_jinja2_file`, ...) over the documents and settings
//! the host sends, so the interpreter only needs `kadet` installed.

use std::path::{Path, PathBuf};

use serde_json::{Value as Json, json};

use super::{Reads, helm};
use crate::python::PythonCmd;
use crate::worker::{Worker, WorkerError};

/// The evaluator and its `kapitan` package, as `(relative path, source)`.
pub const KADET_RUNNER_FILES: &[(&str, &str)] = &[
    (
        "kadet_runner.py",
        include_str!("../../runner/kadet_runner.py"),
    ),
    (
        "kapitan/__init__.py",
        include_str!("../../runner/kapitan/__init__.py"),
    ),
    (
        "kapitan/cached.py",
        include_str!("../../runner/kapitan/cached.py"),
    ),
    (
        "kapitan/defaults.py",
        include_str!("../../runner/kapitan/defaults.py"),
    ),
    (
        "kapitan/errors.py",
        include_str!("../../runner/kapitan/errors.py"),
    ),
    (
        "kapitan/jinja2_filters.py",
        include_str!("../../runner/kapitan/jinja2_filters.py"),
    ),
    (
        "kapitan/resources.py",
        include_str!("../../runner/kapitan/resources.py"),
    ),
    (
        "kapitan/runtime.py",
        include_str!("../../runner/kapitan/runtime.py"),
    ),
    (
        "kapitan/topics.py",
        include_str!("../../runner/kapitan/topics.py"),
    ),
    (
        "kapitan/utils.py",
        include_str!("../../runner/kapitan/utils.py"),
    ),
    (
        "kapitan/version.py",
        include_str!("../../runner/kapitan/version.py"),
    ),
    (
        "kapitan/views.py",
        include_str!("../../runner/kapitan/views.py"),
    ),
    (
        "kapitan/inputs/__init__.py",
        include_str!("../../runner/kapitan/inputs/__init__.py"),
    ),
    (
        "kapitan/inputs/helm.py",
        include_str!("../../runner/kapitan/inputs/helm.py"),
    ),
    (
        "kapitan/inputs/kadet.py",
        include_str!("../../runner/kapitan/inputs/kadet.py"),
    ),
];

fn files_digest() -> String {
    let mut h = blake3::Hasher::new();
    for (path, source) in KADET_RUNNER_FILES {
        h.update(path.as_bytes());
        h.update(b"\0");
        h.update(source.as_bytes());
        h.update(b"\0");
    }
    h.finalize().to_hex().to_string()
}

/// Identifies this build's evaluator (script and package) in the manifest.
pub fn kadet_runner_digest() -> String {
    files_digest()[..16].to_string()
}

/// Write the evaluator and its package to the cache directory (one tree per
/// digest, so every build gets its own copy) and return the script's path.
pub fn materialize_kadet_runner() -> std::io::Result<PathBuf> {
    let base = crate::python::cache_dir()
        .join("kadet-runner")
        .join(&files_digest()[..16]);
    for (rel, source) in KADET_RUNNER_FILES {
        let path = base.join(rel);
        if path.exists() {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, source)?;
    }
    Ok(base.join("kadet_runner.py"))
}

/// Starts evaluator processes. The caller keeps one per target and never
/// hands it to another: modules a component imports stay loaded, with the
/// state they built from the target that imported them first.
pub struct KadetPool {
    python: PythonCmd,
    script: PathBuf,
    init: Json,
    /// The evaluator's working directory (the repository root).
    cwd: PathBuf,
}

impl KadetPool {
    /// `init` is the evaluator's `init` request without `op`: `cwd`,
    /// `inventory_file` or `inventory_socket`, `settings`, `krab_version`.
    pub fn new(python: PythonCmd, init: Json) -> std::io::Result<KadetPool> {
        Ok(KadetPool {
            python,
            script: materialize_kadet_runner()?,
            cwd: init
                .get("cwd")
                .and_then(Json::as_str)
                .map(PathBuf::from)
                .unwrap_or_default(),
            init,
        })
    }

    /// Run `main()` of the component at `input_path` for `target` in
    /// `evaluator`, the target's own, started here when empty.
    #[allow(clippy::too_many_arguments)]
    pub fn eval(
        &self,
        evaluator: &mut Option<Worker>,
        target: &str,
        input_path: &Path,
        input_params: &Json,
        compile_path: &Path,
        temp_dir: &Path,
        reads: &mut Reads,
    ) -> Result<Json, String> {
        if evaluator.is_none() {
            *evaluator = Some(
                Worker::spawn(&self.python, &self.script, self.init.clone())
                    .map_err(|e| format!("cannot start the kadet evaluator: {e}"))?,
            );
        }
        let worker = evaluator.as_mut().unwrap();
        let req = json!({
            "op": "eval",
            "target": target,
            "input_path": input_path,
            "input_params": input_params,
            "compile_path": compile_path,
            "temp_dir": temp_dir,
        });
        // Helm renders the component asks for along the way.
        let mut helm_reads = Reads::default();
        let result = worker.call_with(req, |r| match r.get("op").and_then(Json::as_str) {
            Some("helm") => match serde_json::from_value::<helm::Request>(r.clone())
                .map_err(|e| format!("bad helm request: {e}"))
                .and_then(|q| helm::render(&q, &self.cwd, &mut helm_reads))
            {
                Ok(mut v) => {
                    v["ok"] = json!(true);
                    v
                }
                Err(e) => json!({ "ok": false, "error": e }),
            },
            other => json!({ "ok": false, "error": format!("unsupported host request {other:?}") }),
        });
        reads.extend(helm_reads);
        match result {
            Ok(resp) => {
                for key in ["files", "dirs"] {
                    for p in resp
                        .get(key)
                        .and_then(Json::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(Json::as_str)
                    {
                        if key == "files" {
                            reads.file(Path::new(p))
                        } else {
                            reads.dir(Path::new(p))
                        }
                    }
                }
                for (key, into) in [
                    ("globals", &mut reads.globals),
                    ("doc_reads", &mut reads.doc_keys),
                ] {
                    into.extend(
                        resp.get(key)
                            .and_then(Json::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(Json::as_str)
                            .map(str::to_string),
                    );
                }
                Ok(resp.get("output").cloned().unwrap_or(Json::Null))
            }
            Err(WorkerError::Failed { error, traceback }) => Err(match traceback {
                Some(tb) => format!("{error}\n{tb}"),
                None => error,
            }),
            Err(e) => {
                *evaluator = None;
                Err(format!("kadet evaluator died: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::KADET_RUNNER_FILES;
    use std::path::Path;

    fn python_files(root: &Path, dir: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                python_files(root, &path, out);
            } else if path.extension().is_some_and(|e| e == "py") {
                let rel = path.strip_prefix(root).unwrap();
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    /// A module missing from the list is absent from the evaluator's
    /// `kapitan` package and fails only in a component that imports it.
    #[test]
    fn every_bundled_kapitan_module_is_listed() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("runner");
        let mut on_disk = Vec::new();
        python_files(&root, &root.join("kapitan"), &mut on_disk);
        on_disk.sort();
        let mut listed: Vec<String> = KADET_RUNNER_FILES
            .iter()
            .map(|(path, _)| path.to_string())
            .filter(|path| path.starts_with("kapitan/"))
            .collect();
        listed.sort();
        assert_eq!(listed, on_disk, "KADET_RUNNER_FILES against runner/kapitan");
    }
}

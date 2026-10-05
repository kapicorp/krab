//! What one target needs compiled, derived from its rendered inventory.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::digest::digest_str;
use crate::fetch::normalise_join;

#[derive(Clone, Debug, Serialize)]
pub struct TargetPlan {
    pub name: String,
    /// `a/b/c` for `a.b.c`: the directory under `compiled/`.
    pub target_path: String,
    /// The full rendered target document (parameters, classes, ...).
    pub doc: Value,
    pub doc_digest: String,
    /// `parameters.kapitan.compile`, normalised.
    pub compile: Vec<Value>,
    pub labels: Vec<(String, String)>,
    /// Paths whose existence decides which input files are used; recorded as
    /// dependencies so that creating or removing them re-compiles the target.
    pub probes: Vec<PathBuf>,
}

impl TargetPlan {
    pub fn new(name: &str, doc: Value, search_paths: &[PathBuf]) -> TargetPlan {
        let params = doc.get("parameters").cloned().unwrap_or(Value::Null);
        let kapitan = params.get("kapitan").cloned().unwrap_or(Value::Null);
        let compile: Vec<Value> = kapitan
            .get("compile")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let labels = kapitan
            .get("labels")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            v.as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| v.to_string()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        let mut probes = Vec::new();
        for item in &compile {
            for input in item
                .get("input_paths")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(input) = input.as_str() else {
                    continue;
                };
                if input.contains(['*', '?', '[']) {
                    // Glob: the listing of every directory it can match in.
                    let dir = Path::new(input).parent().unwrap_or(Path::new(""));
                    for sp in search_paths {
                        probes.push(sp.join(dir));
                    }
                } else {
                    for sp in search_paths {
                        probes.push(sp.join(input));
                    }
                }
            }
        }
        probes.sort();
        probes.dedup();
        let doc_digest = digest_str(&serde_json::to_string(&doc).unwrap());
        TargetPlan {
            name: name.to_string(),
            target_path: name.replace('.', "/"),
            doc,
            doc_digest,
            compile,
            labels,
            probes,
        }
    }

    /// An error for the first compile item whose `output_path` leaves
    /// `compiled/<target path>`: only that directory is installed (D14).
    pub fn output_outside_target(&self) -> Option<String> {
        self.compile.iter().find_map(|item| {
            let out = item
                .get("output_path")
                .and_then(Value::as_str)
                .unwrap_or(".");
            (!normalise_join(Path::new(&self.target_path), out).starts_with(&self.target_path))
                .then(|| {
                    format!(
                        "output_path `{out}` resolves outside compiled/{}; a target writes only into its own directory",
                        self.target_path
                    )
                })
        })
    }

    pub fn matches_labels(&self, wanted: &[(String, String)]) -> bool {
        wanted
            .iter()
            .all(|(k, v)| self.labels.iter().any(|(lk, lv)| lk == k && lv == v))
    }
}

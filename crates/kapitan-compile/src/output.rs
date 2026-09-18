//! Writing compiled objects to files the way kapitan's `InputType.to_file`
//! does: prune empties, pick the output type, embed refs, dump.

use std::path::{Path, PathBuf};

use kapitan_inventory::emit::ryml::{RymlOptions, dump_ryml, needs_pyyaml_fallback};
use kapitan_inventory::emit::yaml::{DumpOptions, dump_yaml, dump_yaml_all};
use kapitan_inventory::emit::{MultilineStyle, dumps_pretty};
use kapitan_inventory::{Node, Value};

use crate::digest::{Digests, Fingerprint};
use crate::inputs::Reads;
use crate::refs::{RefController, TargetSecrets};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputType {
    Json,
    Yaml,
    Yml,
    Plain,
    Toml,
    Auto,
}

impl OutputType {
    pub fn parse(s: &str) -> Option<OutputType> {
        Some(match s {
            "json" => OutputType::Json,
            "yaml" => OutputType::Yaml,
            "yml" => OutputType::Yml,
            "plain" => OutputType::Plain,
            "toml" => OutputType::Toml,
            "auto" => OutputType::Auto,
            _ => return None,
        })
    }

    fn ext(self) -> &'static str {
        match self {
            OutputType::Json => "json",
            OutputType::Yaml => "yaml",
            OutputType::Yml => "yml",
            OutputType::Plain => "plain",
            OutputType::Toml => "toml",
            OutputType::Auto => "auto",
        }
    }
}

/// Settings from kapitan's compile arguments that shape every output file.
#[derive(Clone, Debug)]
pub struct WriterOptions {
    pub indent: usize,
    pub use_rapidyaml: bool,
    pub null_as_empty: bool,
    pub multiline: MultilineStyle,
    /// Reveal refs instead of compiling them (`--reveal`).
    pub reveal: bool,
}

impl Default for WriterOptions {
    fn default() -> Self {
        WriterOptions {
            indent: 2,
            use_rapidyaml: false,
            null_as_empty: false,
            multiline: MultilineStyle::Literal,
            reveal: false,
        }
    }
}

pub struct Writer<'a> {
    pub opts: WriterOptions,
    pub refs: &'a RefController,
    /// The target's `parameters.kapitan.secrets`, for refs created from functions.
    pub target: TargetSecrets,
}

impl Writer<'_> {
    /// Compile the refs in a string, or reveal them under `--reveal`.
    pub fn refs_str(&self, s: &str, reads: &mut Reads) -> Result<String, String> {
        if self.opts.reveal {
            self.refs.reveal_str(s, reads)
        } else {
            self.refs.compile_str(s, &self.target, reads)
        }
        .map_err(|e| e.to_string())
    }

    /// Compile the refs in every string of a value tree, or reveal them under `--reveal`.
    pub fn refs_value(&self, v: &mut Value, reads: &mut Reads) -> Result<(), String> {
        if self.opts.reveal {
            self.refs.reveal_value(v, reads)
        } else {
            self.refs.compile_value(v, &self.target, reads)
        }
        .map_err(|e| e.to_string())
    }

    /// `to_file`: `file_path` has no extension yet; the output type decides
    /// it. Returns the path written and its content fingerprint (or `None`
    /// when kapitan would skip an empty document).
    pub fn to_file(
        &self,
        output_type: OutputType,
        default_type: OutputType,
        prune: bool,
        file_path: &Path,
        mut content: Value,
        reads: &mut Reads,
    ) -> Result<Option<(PathBuf, Fingerprint)>, String> {
        if prune {
            content = prune_empty(content).unwrap_or(Value::Null);
        }
        let (output_type, ext): (OutputType, Option<&str>) = match output_type {
            OutputType::Auto => match file_path.extension().and_then(|e| e.to_str()) {
                Some(e @ ("toml" | "json" | "yaml" | "yml")) => {
                    (OutputType::parse(e).unwrap(), None)
                }
                _ => (default_type, Some(default_type.ext())),
            },
            OutputType::Plain => (OutputType::Plain, None),
            t => (t, Some(t.ext())),
        };
        let path = match ext {
            Some(e) => {
                let mut s = file_path.as_os_str().to_owned();
                s.push(".");
                s.push(e);
                PathBuf::from(s)
            }
            None => file_path.to_path_buf(),
        };
        let text = match output_type {
            OutputType::Plain => {
                let s = match &content {
                    Value::Str(s) => s.clone(),
                    other => other.py_str(),
                };
                self.refs_str(&s, reads)?
            }
            OutputType::Json => {
                self.refs_value(&mut content, reads)?;
                if !content.truthy() {
                    return Ok(None);
                }
                dumps_pretty(&content, self.opts.indent, true)
            }
            OutputType::Yaml | OutputType::Yml => {
                self.refs_value(&mut content, reads)?;
                if !content.truthy() {
                    return Ok(None);
                }
                self.yaml(&content)
            }
            OutputType::Toml => {
                return Err("toml output is not supported by the native compiler yet".into());
            }
            OutputType::Auto => unreachable!(),
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // Hashed from memory: a fresh file written this way has no exec bit.
        let fingerprint = Digests::of_bytes(false, text.as_bytes());
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(Some((path, fingerprint)))
    }

    /// `write_yaml`: a list at the top becomes a multi-document stream.
    pub fn yaml(&self, content: &Value) -> String {
        let node = Node::synthetic(content.clone());
        let multi = !matches!(content, Value::Map(_));
        if self.opts.use_rapidyaml && !needs_pyyaml_fallback(content) {
            return dump_ryml(
                &node,
                &RymlOptions {
                    multiline: self.opts.multiline,
                    null_as_empty: self.opts.null_as_empty,
                },
                multi,
            );
        }
        let opts = DumpOptions {
            indent: self.opts.indent,
            multiline: Some(self.opts.multiline),
            // PyYAML's null representer honours the flag too.
            null_as_empty: self.opts.null_as_empty,
            ..DumpOptions::default()
        };
        match (&node.value, multi) {
            (Value::List(items), true) => dump_yaml_all(items, &opts),
            _ => dump_yaml(&node, &opts),
        }
    }
}

/// kapitan `prune_empty`: drop `None` items and empty containers, recursively.
/// Returns `None` for an empty container (callers drop it).
pub fn prune_empty(v: Value) -> Option<Value> {
    match v {
        Value::List(l) => {
            if l.is_empty() {
                return None;
            }
            Some(Value::List(
                l.into_iter()
                    .filter_map(|n| match n.value {
                        Value::Null => None,
                        other => prune_empty(other).map(|v| Node::new(v, n.origin)),
                    })
                    .collect(),
            ))
        }
        Value::Map(m) => {
            if m.is_empty() {
                return None;
            }
            Some(Value::Map(
                m.into_iter()
                    .filter_map(|(k, n)| match n.value {
                        Value::Null => None,
                        other => prune_empty(other).map(|v| (k, Node::new(v, n.origin))),
                    })
                    .collect(),
            ))
        }
        other => Some(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kapitan_inventory::source::SourceId;
    use kapitan_inventory::yaml::parse_document;

    #[test]
    fn prunes_like_kapitan() {
        let v = parse_document(
            "a: {b: null, c: [], d: [null, 1, {}], e: {f: {}}}\ng: 0\n",
            SourceId(0),
        )
        .unwrap()
        .value;
        let pruned = prune_empty(v).unwrap();
        // A mapping emptied by pruning stays (only mappings that were empty go).
        assert_eq!(
            serde_json::to_string(&pruned).unwrap(),
            r#"{"a":{"d":[1],"e":{}},"g":0}"#
        );
    }
}

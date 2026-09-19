//! `jinja2`: render a file or a directory of templates with minijinja,
//! matching the Jinja2 environment kapitan builds (strict undefined,
//! trim_blocks, lstrip_blocks, kapitan's filters, Python-style rendering of
//! booleans and `None`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::{Node, Value};
use minijinja::value::{Enumerator, Object, ObjectRepr, Value as JValue, ValueKind};
use minijinja::{AutoEscape, Environment, Error, ErrorKind, UndefinedBehavior};

use crate::docs::SharedDocs;
use crate::refs::RefController;
use serde_json::Value as Json;
use std::error::Error as _;

use super::Reads;

/// One rendered file: content plus the source file's mode bits.
pub struct Rendered {
    pub name: String,
    pub content: String,
    pub mode: u32,
}

pub struct JinjaContext<'a> {
    pub target: &'a str,
    pub inventory: &'a Json,
    /// Other targets, fetched when a template asks for them.
    pub docs: SharedDocs,
    pub input_params: &'a Json,
    pub search_paths: &'a [PathBuf],
    pub reveal: bool,
    /// Refs, for the `reveal_maybe` filter.
    pub refs: Arc<RefController>,
}

/// `inventory_global` as seen by templates: a mapping whose entries are
/// fetched on access, recording which targets were read.
struct LazyGlobal {
    docs: SharedDocs,
    accessed: parking_lot::Mutex<Vec<String>>,
}

impl std::fmt::Debug for LazyGlobal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("inventory_global")
    }
}

impl Object for LazyGlobal {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &JValue) -> Option<JValue> {
        let name = key.as_str()?;
        self.accessed.lock().push(name.to_string());
        self.docs.get(name).map(|d| JValue::from_serialize(&d))
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        self.accessed.lock().push("*".into());
        Enumerator::Values(self.docs.names().into_iter().map(JValue::from).collect())
    }
}

/// kapitan `render_jinja2`: a file, or every non-hidden file under a directory.
pub fn render(
    input: &Path,
    ctx: &JinjaContext,
    reads: &mut Reads,
) -> Result<Vec<Rendered>, String> {
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    if input.is_file() {
        files.push((
            input.file_name().unwrap().to_string_lossy().to_string(),
            input.to_path_buf(),
        ));
    } else {
        walk(input, input, &mut files, reads)?;
    }
    let mut out = Vec::new();
    for (name, path) in files {
        reads.file(&path);
        let content = render_file(&path, ctx, reads)?;
        let mode = std::fs::metadata(&path)
            .map(|m| {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    m.permissions().mode() & 0o7777
                }
                #[cfg(not(unix))]
                {
                    0o644
                }
            })
            .unwrap_or(0o644);
        out.push(Rendered {
            name,
            content,
            mode,
        });
    }
    Ok(out)
}

fn walk(
    root: &Path,
    dir: &Path,
    out: &mut Vec<(String, PathBuf)>,
    reads: &mut Reads,
) -> Result<(), String> {
    reads.dir(dir);
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .flatten()
        .collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            walk(root, &path, out, reads)?;
        } else if !name.starts_with('.') {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

fn render_file(path: &Path, ctx: &JinjaContext, reads: &mut Reads) -> Result<String, String> {
    let source = std::fs::read_to_string(path)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let dir = path.parent().unwrap_or(Path::new(".")).to_path_buf();
    let mut loader_paths = vec![dir];
    loader_paths.extend(ctx.search_paths.iter().cloned());
    let loaded_files = Arc::new(parking_lot::Mutex::new(Vec::<PathBuf>::new()));
    // Ref files the `reveal_maybe` filter reads.
    let ref_reads = Arc::new(parking_lot::Mutex::new(Reads::default()));

    let mut env = Environment::new();
    // Jinja2 does not escape by default; minijinja would JSON-escape .yaml/.json templates.
    env.set_auto_escape_callback(|_| AutoEscape::None);
    env.set_undefined_behavior(UndefinedBehavior::Strict);
    env.set_trim_blocks(true);
    env.set_lstrip_blocks(true);
    env.set_keep_trailing_newline(false);
    let lf = loaded_files.clone();
    env.set_loader(move |name: &str| {
        for base in &loader_paths {
            let candidate = base.join(name);
            if candidate.is_file() {
                lf.lock().push(candidate.clone());
                return std::fs::read_to_string(&candidate)
                    .map(Some)
                    .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()));
            }
        }
        Ok(None)
    });
    env.set_formatter(python_formatter);
    register_filters(&mut env, ctx.reveal, ctx.refs.clone(), ref_reads.clone());

    let template_name = path.file_name().unwrap().to_string_lossy().to_string();
    env.add_template_owned(template_name.clone(), source)
        .map_err(|e| format!("{}: {e}", path.display()))?;
    let tmpl = env
        .get_template(&template_name)
        .map_err(|e| e.to_string())?;

    let global = Arc::new(LazyGlobal {
        docs: ctx.docs.clone(),
        accessed: parking_lot::Mutex::new(Vec::new()),
    });
    let mut context: std::collections::BTreeMap<String, JValue> = std::collections::BTreeMap::new();
    context.insert(
        "inventory_global".into(),
        JValue::from_dyn_object(global.clone()),
    );
    context.insert("inventory".into(), JValue::from_serialize(ctx.inventory));
    context.insert(
        "input_params".into(),
        JValue::from_serialize(ctx.input_params),
    );
    if let Some(vars) = ctx
        .inventory
        .pointer("/parameters/kapitan/vars")
        .and_then(Json::as_object)
    {
        for (k, v) in vars {
            context.insert(k.clone(), JValue::from_serialize(v));
        }
    }
    let rendered = tmpl.render(JValue::from_serialize(&context)).map_err(|e| {
        format!(
            "Jinja2 TemplateError: {} in {}",
            describe(&e),
            path.display()
        )
    })?;
    for f in loaded_files.lock().drain(..) {
        reads.file(&f);
    }
    reads.extend(std::mem::take(&mut *ref_reads.lock()));
    reads.globals.extend(global.accessed.lock().drain(..));
    Ok(rendered)
}

fn describe(e: &Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(&format!(": {c}"));
        cur = c.source();
    }
    s
}

/// Jinja2 renders Python values: `True`/`False`/`None`, dict/list reprs.
fn python_formatter(
    out: &mut minijinja::Output,
    state: &minijinja::State,
    value: &JValue,
) -> Result<(), Error> {
    match value.kind() {
        ValueKind::Bool => {
            out.write_str(if value.is_true() { "True" } else { "False" })?;
            Ok(())
        }
        ValueKind::None => {
            out.write_str("None")?;
            Ok(())
        }
        ValueKind::Seq | ValueKind::Map | ValueKind::Number
            if matches!(value.kind(), ValueKind::Seq | ValueKind::Map) =>
        {
            let v: Value = serde_json::to_value(value)
                .map(Into::into)
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            out.write_str(&v.py_repr())?;
            Ok(())
        }
        _ => minijinja::escape_formatter(out, state, value),
    }
}

fn to_value(v: &JValue) -> Result<Value, Error> {
    serde_json::to_value(v)
        .map(Into::into)
        .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))
}

fn register_filters(
    env: &mut Environment<'_>,
    reveal: bool,
    refs: Arc<RefController>,
    ref_reads: Arc<parking_lot::Mutex<Reads>>,
) {
    env.add_filter("sha256", |s: String| {
        hex::encode(<sha2::Sha256 as sha2::Digest>::digest(s.as_bytes()))
    });
    env.add_filter("b64encode", |s: String| {
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    });
    env.add_filter("b64decode", |s: String| -> Result<String, Error> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(s.as_bytes())
            .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
        String::from_utf8(bytes).map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))
    });
    env.add_filter("yaml", |v: JValue| -> Result<String, Error> {
        let value = to_value(&v)?;
        Ok(dump_yaml(
            &Node::synthetic(value),
            &DumpOptions::pyyaml_default(),
        ))
    });
    env.add_filter("to_json", |v: JValue| -> Result<String, Error> {
        // The common user filter: json.dumps(obj, ensure_ascii=False, indent=4)
        let value = to_value(&v)?;
        Ok(json_indent4(&value))
    });
    env.add_filter("basename", |s: String| {
        Path::new(&s)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    env.add_filter("dirname", |s: String| {
        let p = Path::new(&s);
        p.parent()
            .map(|d| d.to_string_lossy().to_string())
            .unwrap_or_default()
    });
    env.add_filter("bool", |v: JValue| -> JValue {
        match v.kind() {
            ValueKind::None | ValueKind::Bool => v,
            _ => {
                let s = v.to_string().to_lowercase();
                JValue::from(matches!(s.as_str(), "yes" | "on" | "1" | "true"))
            }
        }
    });
    env.add_filter(
        "ternary",
        |v: JValue, t: JValue, f: JValue, none_val: Option<JValue>| -> JValue {
            if v.is_none()
                && let Some(n) = none_val
            {
                return n;
            }
            if v.is_true() { t } else { f }
        },
    );
    env.add_filter(
        "regex_replace",
        |value: String,
         pattern: String,
         replacement: String,
         ignorecase: Option<bool>|
         -> Result<String, Error> {
            let re = regex::RegexBuilder::new(&pattern)
                .case_insensitive(ignorecase.unwrap_or(false))
                .build()
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            Ok(re
                .replace_all(&value, python_replacement(&replacement).as_str())
                .to_string())
        },
    );
    env.add_filter("regex_escape", |s: String| regex::escape(&s));
    env.add_filter(
        "regex_search",
        |value: String, pattern: String| -> Result<JValue, Error> {
            let re = regex::Regex::new(&pattern)
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            Ok(re
                .find(&value)
                .map(|m| JValue::from(m.as_str()))
                .unwrap_or(JValue::from(())))
        },
    );
    env.add_filter(
        "regex_findall",
        |value: String, pattern: String| -> Result<JValue, Error> {
            let re = regex::Regex::new(&pattern)
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()))?;
            Ok(JValue::from(
                re.find_iter(&value)
                    .map(|m| m.as_str().to_string())
                    .collect::<Vec<_>>(),
            ))
        },
    );
    env.add_filter("reveal_maybe", move |s: String| -> Result<String, Error> {
        if reveal {
            return refs
                .reveal_str(&s, &mut ref_reads.lock())
                .map_err(|e| Error::new(ErrorKind::InvalidOperation, e.to_string()));
        }
        Ok(s)
    });
    env.add_filter("fileglob", |pattern: String| -> JValue {
        let mut files: Vec<String> = glob::glob(&pattern)
            .map(|g| {
                g.flatten()
                    .filter(|p| p.is_file())
                    .map(|p| p.to_string_lossy().to_string())
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        JValue::from(files)
    });
    env.add_filter("merge_strategic", |v: JValue| -> Result<JValue, Error> {
        let value = to_value(&v)?;
        Ok(JValue::from_serialize(merge_strategic(value)))
    });
    for unsupported in ["toml", "to_datetime", "strftime", "shuffle"] {
        let name = unsupported.to_string();
        env.add_filter(unsupported, move |_v: JValue| -> Result<JValue, Error> {
            Err(Error::new(
                ErrorKind::InvalidOperation,
                format!("the `{name}` filter is not supported by the native compiler yet"),
            ))
        });
    }
}

/// Python `\1` / `\g<name>` back-references to the regex crate's `$1` / `$name`.
fn python_replacement(r: &str) -> String {
    let re = regex::Regex::new(r"\\g<(\w+)>|\\(\d+)").unwrap();
    let escaped = r.replace('$', "$$");
    re.replace_all(&escaped, |c: &regex::Captures| {
        let name = c.get(1).or(c.get(2)).map(|m| m.as_str()).unwrap_or("");
        format!("${{{name}}}")
    })
    .to_string()
}

fn json_indent4(v: &Value) -> String {
    let mut s = krab_inventory::emit::dumps_pretty(v, 4, false);
    // ensure_ascii=False: undo the \uXXXX escapes for non-ASCII.
    if s.contains("\\u")
        && let Ok(parsed) = serde_json::from_str::<Json>(&s)
    {
        s = serde_json::to_string_pretty(&parsed).unwrap_or(s);
    }
    s
}

/// kapitan's `merge_strategic`: lists of dicts with a `name` merge by name.
fn merge_strategic(v: Value) -> Value {
    match v {
        Value::List(items) => {
            let processed: Vec<Node> = items
                .into_iter()
                .map(|n| Node::new(merge_strategic(n.value), n.origin))
                .collect();
            let named = processed
                .iter()
                .all(|n| n.as_map().is_some_and(|m| m.contains_key("name")));
            if named && !processed.is_empty() {
                let mut merged: krab_inventory::Map = Default::default();
                for n in processed {
                    let Value::Map(m) = n.value else {
                        unreachable!()
                    };
                    let key = m["name"].value.py_str();
                    let entry = merged
                        .entry(key)
                        .or_insert_with(|| Node::synthetic(Value::Map(Default::default())));
                    if let Value::Map(target) = &mut entry.value {
                        for (k, v) in m {
                            target.insert(k, v);
                        }
                    }
                }
                Value::List(merged.into_values().collect())
            } else {
                Value::List(processed)
            }
        }
        Value::Map(m) => Value::Map(
            m.into_iter()
                .map(|(k, n)| (k, Node::new(merge_strategic(n.value), n.origin)))
                .collect(),
        ),
        other => other,
    }
}

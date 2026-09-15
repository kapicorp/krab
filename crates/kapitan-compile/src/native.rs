//! The native compiler: one target, all its compile items, written straight
//! into the target's temporary tree. Only `kadet` evaluates Python (through
//! the evaluator pool); everything else is Rust.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kapitan_inventory::Value;
use kapitan_inventory::emit::MultilineStyle;
use serde_json::{Value as Json, json};

use crate::digest::{Digests, digest_str};
use crate::docs::SharedDocs;
use crate::inputs::jinja::JinjaContext;
use crate::inputs::kadet::KadetPool;
use crate::inputs::{Item, Reads, copy, external, jinja, remove, resolve_input_paths};
use crate::manifest::ItemRecord;
use crate::output::{OutputType, Writer, WriterOptions};
use crate::plan::TargetPlan;
use crate::python::PythonCmd;
use crate::refs::{RefController, TargetSecrets};

#[derive(Clone, Debug)]
pub struct NativeOptions {
    pub repo_root: PathBuf,
    pub search_paths: Vec<PathBuf>,
    pub refs_path: PathBuf,
    pub embed_refs: bool,
    pub reveal: bool,
    pub indent: usize,
    pub use_rapidyaml: bool,
    pub null_as_empty: bool,
    /// Used unless the target sets `parameters.multiline_string_style`.
    pub multiline: MultilineStyle,
}

pub struct CompileOutcome {
    pub reads: Reads,
    pub warnings: Vec<String>,
    /// The kadet items, in compile order.
    pub items: Vec<ItemRecord>,
    /// How many of them were copied from the previous output instead of evaluated.
    pub reused: usize,
}

/// What deciding whether a kadet item's previous output is still valid needs.
pub struct ItemContext<'a> {
    /// The target's items from the last compile with the same compiler and settings.
    pub previous: &'a [ItemRecord],
    /// Fingerprints of every path the last compile recorded.
    pub files: &'a BTreeMap<String, String>,
    pub all_digests: &'a BTreeMap<String, String>,
    pub everything_digest: &'a str,
    pub compiled_dir: &'a Path,
    pub digests: &'a Digests,
    pub repo_root: &'a Path,
}

/// Per-target bookkeeping of the kadet items while compiling.
#[derive(Default)]
struct ItemRecords {
    items: Vec<ItemRecord>,
    reused: usize,
    /// Previous items already matched to a current one.
    matched: Vec<bool>,
}

pub struct NativeCompiler {
    pub opts: NativeOptions,
    refs: Arc<RefController>,
    kadet: KadetPool,
    docs: SharedDocs,
}

impl NativeCompiler {
    /// Generators and templates read other targets through `docs`; the kadet
    /// evaluator does so through the server `socket` when there is one, and
    /// from `inventory_file` (every document) otherwise.
    pub fn new(
        opts: NativeOptions,
        python: PythonCmd,
        socket: Option<&Path>,
        inventory_file: &Path,
        flags: &[String],
        docs: SharedDocs,
    ) -> std::io::Result<Self> {
        let init = json!({
            "cwd": opts.repo_root,
            "inventory_file": inventory_file,
            "inventory_socket": socket,
            "search_paths": opts.search_paths,
            "flags": flags,
        });
        let refs = Arc::new(RefController::new(opts.refs_path.clone(), opts.embed_refs));
        let kadet = KadetPool::new(python, init)?;
        Ok(NativeCompiler {
            opts,
            refs,
            kadet,
            docs,
        })
    }

    pub fn compile_target(
        &self,
        plan: &TargetPlan,
        temp_dir: &Path,
        items: &ItemContext,
    ) -> Result<CompileOutcome, String> {
        let mut reads = Reads::default();
        let mut warnings = Vec::new();
        let mut records = ItemRecords {
            matched: vec![false; items.previous.len()],
            ..Default::default()
        };
        let multiline = plan
            .doc
            .pointer("/parameters/multiline_string_style")
            .and_then(Json::as_str)
            .and_then(parse_style)
            .unwrap_or(self.opts.multiline);
        let writer = Writer {
            opts: WriterOptions {
                indent: self.opts.indent,
                use_rapidyaml: self.opts.use_rapidyaml,
                null_as_empty: self.opts.null_as_empty,
                multiline,
                reveal: self.opts.reveal,
            },
            refs: &self.refs,
            target: TargetSecrets::from_document(&plan.name, &plan.doc),
        };
        let compile_root = temp_dir.join("compiled");
        for raw in &plan.compile {
            let item = Item::from_json(raw)?;
            let target_compile_path = compile_root.join(&plan.target_path).join(&item.output_path);
            std::fs::create_dir_all(&target_compile_path).map_err(|e| e.to_string())?;
            let result = self.compile_item(
                &item,
                plan,
                &compile_root,
                &target_compile_path,
                temp_dir,
                &writer,
                &mut reads,
                items,
                &mut records,
            );
            if let Err(e) = result {
                if item.continue_on_error {
                    warnings.push(format!("{} {:?}: {e}", item.input_type, item.input_paths));
                    continue;
                }
                return Err(format!("{} {:?}: {e}", item.input_type, item.input_paths));
            }
        }
        Ok(CompileOutcome {
            reads,
            warnings,
            items: records.items,
            reused: records.reused,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn compile_item(
        &self,
        item: &Item,
        plan: &TargetPlan,
        compile_root: &Path,
        target_compile_path: &Path,
        temp_dir: &Path,
        writer: &Writer,
        reads: &mut Reads,
        ctx: &ItemContext,
        records: &mut ItemRecords,
    ) -> Result<(), String> {
        let output_type = OutputType::parse(&item.output_type)
            .ok_or_else(|| format!("unknown output_type `{}`", item.output_type))?;
        let mut search_paths = self.opts.search_paths.clone();
        search_paths.push(temp_dir.to_path_buf());
        let inputs = resolve_input_paths(item, &search_paths, &plan.name, reads)?;
        match item.input_type.as_str() {
            "kadet" => {
                let item_digest = digest_str(&item.raw.to_string());
                for input in inputs {
                    let mut item_reads = Reads::default();
                    let record = match reusable(ctx, &item_digest, plan, &mut records.matched) {
                        Some(previous) => {
                            restore_outputs(previous, ctx.compiled_dir, compile_root)?;
                            for rel in &previous.deps {
                                item_reads.file(&self.opts.repo_root.join(rel));
                            }
                            item_reads.globals.extend(previous.globals.keys().cloned());
                            records.reused += 1;
                            previous.clone()
                        }
                        None => {
                            let output = self.kadet.eval(
                                &plan.name,
                                &input,
                                &item.input_params,
                                target_compile_path,
                                temp_dir,
                                &mut item_reads,
                            )?;
                            let mut outputs = BTreeMap::new();
                            if let Json::Object(files) = output {
                                for (key, value) in files {
                                    let written = writer.to_file(
                                        output_type,
                                        OutputType::Yaml,
                                        item.prune,
                                        &target_compile_path.join(&key),
                                        Value::from(value),
                                        &mut item_reads,
                                    )?;
                                    if let Some((path, fingerprint)) = written {
                                        outputs.insert(relative(&path, compile_root), fingerprint);
                                    }
                                }
                            }
                            item_record(&item_digest, plan, &item_reads, outputs, ctx)
                        }
                    };
                    reads.extend(item_reads);
                    records.items.push(record);
                }
            }
            "jinja2" => {
                let ctx = JinjaContext {
                    target: &plan.name,
                    inventory: &plan.doc,
                    docs: self.docs.clone(),
                    input_params: &with_compile_path(&item.input_params, target_compile_path),
                    search_paths: &search_paths,
                    reveal: self.opts.reveal,
                    refs: self.refs.clone(),
                };
                let strip = item
                    .raw
                    .get("suffix_remove")
                    .and_then(Json::as_bool)
                    .unwrap_or(false);
                let suffix = item
                    .raw
                    .get("suffix_stripped")
                    .and_then(Json::as_str)
                    .unwrap_or(".j2");
                for input in inputs {
                    for rendered in jinja::render(&input, &ctx, reads)? {
                        let mut name = rendered.name.clone();
                        if strip && name.ends_with(suffix) {
                            // Python's `str.rstrip(chars)`, quirk included.
                            name = name.trim_end_matches(|c| suffix.contains(c)).to_string();
                        }
                        let path = target_compile_path.join(&name);
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
                        }
                        let content = writer.refs_str(&rendered.content, reads)?;
                        std::fs::write(&path, content)
                            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            let _ = std::fs::set_permissions(
                                &path,
                                std::fs::Permissions::from_mode(rendered.mode),
                            );
                        }
                    }
                }
            }
            "copy" => {
                for input in inputs {
                    copy::compile(&input, target_compile_path, item.ignore_missing, reads)?;
                }
            }
            "remove" => {
                for input in inputs {
                    remove::compile(&input)?;
                }
            }
            "external" => {
                for input in inputs {
                    external::compile(&input, target_compile_path, &item.raw)?;
                }
            }
            other => {
                if inputs.is_empty() {
                    return Ok(());
                }
                return Err(format!(
                    "input type `{other}` is not supported by the native compiler yet (use --backend python)"
                ));
            }
        }
        let _ = compile_root;
        Ok(())
    }
}

fn relative(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace(std::path::MAIN_SEPARATOR, "/")
}

/// Digest of the part of the target document a `doc_reads` key names.
fn doc_read_digest(plan: &TargetPlan, key: &str) -> String {
    if key == "*" {
        return plan.doc_digest.clone();
    }
    let value = match key.strip_prefix("parameters.") {
        Some(k) => plan.doc.get("parameters").and_then(|p| p.get(k)),
        None => plan.doc.get(key),
    };
    value
        .map(|v| digest_str(&v.to_string()))
        .unwrap_or_else(|| "-".into())
}

/// The previous item with this definition, if everything it depended on is
/// as it was: the document parts it read, its files, the other targets it
/// read and its own output.
fn reusable<'a>(
    ctx: &'a ItemContext,
    item_digest: &str,
    plan: &TargetPlan,
    matched: &mut [bool],
) -> Option<&'a ItemRecord> {
    let (i, previous) = ctx
        .previous
        .iter()
        .enumerate()
        .find(|(i, p)| !matched[*i] && p.item_digest == item_digest)?;
    matched[i] = true;
    let outputs = Digests::new();
    let unchanged = previous
        .doc_reads
        .iter()
        .all(|(key, d)| doc_read_digest(plan, key) == *d)
        && previous.deps.iter().all(|rel| {
            ctx.files
                .get(rel)
                .is_some_and(|fp| ctx.digests.fingerprint(&ctx.repo_root.join(rel)) == *fp)
        })
        && previous
            .globals
            .iter()
            .all(|(name, d)| match name.as_str() {
                "*" => d == ctx.everything_digest,
                _ => ctx.all_digests.get(name) == Some(d),
            })
        && previous
            .outputs
            .iter()
            .all(|(rel, fp)| outputs.fingerprint(&ctx.compiled_dir.join(rel)) == *fp);
    unchanged.then_some(previous)
}

fn restore_outputs(
    previous: &ItemRecord,
    compiled_dir: &Path,
    compile_root: &Path,
) -> Result<(), String> {
    for rel in previous.outputs.keys() {
        let from = compiled_dir.join(rel);
        let to = compile_root.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        // The staging tree lives next to `compiled/`, so a hard link is
        // enough: install later renames it back over the original.
        if std::fs::hard_link(&from, &to).is_err() {
            std::fs::copy(&from, &to)
                .map_err(|e| format!("cannot reuse {}: {e}", from.display()))?;
        }
    }
    Ok(())
}

fn item_record(
    item_digest: &str,
    plan: &TargetPlan,
    reads: &Reads,
    outputs: BTreeMap<String, String>,
    ctx: &ItemContext,
) -> ItemRecord {
    let mut doc_reads = BTreeMap::new();
    if reads.doc_keys.iter().any(|k| k == "*") {
        doc_reads.insert("*".to_string(), plan.doc_digest.clone());
    } else {
        for key in &reads.doc_keys {
            doc_reads.insert(key.clone(), doc_read_digest(plan, key));
        }
    }
    let mut deps: Vec<String> = reads
        .files
        .iter()
        .chain(&reads.dirs)
        .filter_map(|p| p.strip_prefix(ctx.repo_root).ok())
        .map(|rel| relative(rel, Path::new("")))
        .collect();
    deps.sort();
    deps.dedup();
    let mut globals = BTreeMap::new();
    if reads.globals.iter().any(|g| g == "*") {
        globals.insert("*".to_string(), ctx.everything_digest.to_string());
    } else {
        for g in &reads.globals {
            if g != &plan.name
                && let Some(d) = ctx.all_digests.get(g)
            {
                globals.insert(g.clone(), d.clone());
            }
        }
    }
    ItemRecord {
        item_digest: item_digest.to_string(),
        doc_reads,
        deps,
        globals,
        outputs,
    }
}

fn with_compile_path(params: &Json, compile_path: &Path) -> Json {
    let mut p = params.clone();
    if let Json::Object(m) = &mut p {
        m.entry("compile_path")
            .or_insert_with(|| Json::String(compile_path.to_string_lossy().to_string()));
    }
    p
}

pub fn parse_style(s: &str) -> Option<MultilineStyle> {
    match s {
        "literal" => Some(MultilineStyle::Literal),
        "folded" => Some(MultilineStyle::Folded),
        "double-quotes" => Some(MultilineStyle::DoubleQuotes),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> TargetPlan {
        TargetPlan::new(
            "a.b",
            json!({"parameters": {"x": {"k": 1}, "y": [1, 2]}, "classes": ["c"]}),
            &[],
        )
    }

    fn ctx<'a>(
        previous: &'a [ItemRecord],
        files: &'a BTreeMap<String, String>,
        all: &'a BTreeMap<String, String>,
        digests: &'a Digests,
        root: &'a Path,
    ) -> ItemContext<'a> {
        ItemContext {
            previous,
            files,
            all_digests: all,
            everything_digest: "every",
            compiled_dir: root,
            digests,
            repo_root: root,
        }
    }

    #[test]
    fn document_part_digests() {
        let p = plan();
        assert_eq!(doc_read_digest(&p, "*"), p.doc_digest);
        assert_eq!(
            doc_read_digest(&p, "parameters.x"),
            digest_str(r#"{"k":1}"#)
        );
        assert_eq!(doc_read_digest(&p, "classes"), digest_str(r#"["c"]"#));
        assert_eq!(doc_read_digest(&p, "parameters.missing"), "-");
    }

    #[test]
    fn reuse_needs_every_input_unchanged() {
        let p = plan();
        let dir = std::env::temp_dir().join(format!("krab-reuse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("out.yaml"), "a: 1\n").unwrap();
        let digests = Digests::new();
        let files = BTreeMap::new();
        let all = BTreeMap::from([("other".to_string(), "d1".to_string())]);
        let record = |doc_reads: &[(&str, &str)], globals: &[(&str, &str)]| ItemRecord {
            item_digest: "item".into(),
            doc_reads: doc_reads
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            deps: vec![],
            globals: globals
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            outputs: BTreeMap::from([(
                "out.yaml".to_string(),
                digests.fingerprint(&dir.join("out.yaml")),
            )]),
        };
        let x = doc_read_digest(&p, "parameters.x");
        let good = [record(&[("parameters.x", &x)], &[("other", "d1")])];
        let c = ctx(&good, &files, &all, &digests, &dir);
        assert!(reusable(&c, "item", &p, &mut [false]).is_some());
        assert!(reusable(&c, "different-item", &p, &mut [false]).is_none());
        // An item already matched is not offered twice.
        assert!(reusable(&c, "item", &p, &mut [true]).is_none());

        let stale_doc = [record(&[("parameters.x", "old")], &[])];
        assert!(
            reusable(
                &ctx(&stale_doc, &files, &all, &digests, &dir),
                "item",
                &p,
                &mut [false]
            )
            .is_none()
        );
        let stale_global = [record(&[], &[("other", "d0")])];
        assert!(
            reusable(
                &ctx(&stale_global, &files, &all, &digests, &dir),
                "item",
                &p,
                &mut [false]
            )
            .is_none()
        );
        let everything = [record(&[], &[("*", "every")])];
        assert!(
            reusable(
                &ctx(&everything, &files, &all, &digests, &dir),
                "item",
                &p,
                &mut [false]
            )
            .is_some()
        );

        std::fs::write(dir.join("out.yaml"), "edited\n").unwrap();
        let fresh = Digests::new();
        let c = ctx(&good, &files, &all, &fresh, &dir);
        assert!(
            reusable(&c, "item", &p, &mut [false]).is_none(),
            "modified output is not reused"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! The inventory: target discovery, class resolution and rendering.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;
use rayon::prelude::*;
use serde::Serialize;

use crate::classfile::ClassDoc;
use crate::error::{Diagnostic, Error, Result};
use crate::interp::eval::{Evaluator, ResolveEvent};
use crate::merge::{MergeDeref, MergeEvent, merge};
use crate::model;
use crate::path::KeyPath;
use crate::resolvers::Registry;
use crate::resolvers::builtin::process_literals;
use crate::source::{Origin, Sources};
use crate::value::{Node, Value};
use crate::yaml;

#[derive(Clone, Debug)]
pub struct InventoryConfig {
    /// The inventory directory (contains `targets/` and `classes/`).
    pub root: PathBuf,
    /// Name targets after their path (`gcp.project.foo`) instead of the file
    /// name. Off by default, as in the reference (`global.compose-target-name`,
    /// or the older `compile.compose-node-name`).
    pub compose_target_name: bool,
    pub ignore_class_not_found: bool,
    /// Record merge and resolution history for `explain`.
    pub track_provenance: bool,
    /// Apply kapitan's typed normalisation of `parameters.kapitan`.
    pub normalize: bool,
    /// Resolution passes (the reference implementation makes three).
    pub passes: usize,
}

impl InventoryConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        InventoryConfig {
            root: root.into(),
            compose_target_name: false,
            ignore_class_not_found: false,
            track_provenance: true,
            normalize: true,
            passes: 3,
        }
    }

    pub fn targets_dir(&self) -> PathBuf {
        self.root.join("targets")
    }

    pub fn classes_dir(&self) -> PathBuf {
        self.root.join("classes")
    }
}

/// A target file found under `targets/`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq, PartialOrd, Ord)]
pub struct TargetSpec {
    pub name: String,
    /// Path relative to `targets/`, with extension.
    pub path: String,
    pub file: PathBuf,
}

impl TargetSpec {
    /// The file's path as a dotted name (`prod/app.yml` -> `prod.app`). It is
    /// the target's name when `compose-target-name` is set, and stays usable
    /// for selecting a target either way.
    pub fn dotted_path(&self) -> String {
        self.path
            .rsplit_once('.')
            .map(|(stem, _)| stem)
            .unwrap_or(&self.path)
            .replace('/', ".")
    }
}

/// Outcome of resolving a class name: the file found (if any) and every
/// candidate path that was checked on the way.
#[derive(Clone, Debug)]
pub struct ClassResolution {
    pub found: Option<PathBuf>,
    pub tried: Vec<PathBuf>,
}

/// A parsed file, cached by path with its content digest.
pub struct LoadedFile {
    pub path: PathBuf,
    pub digest: [u8; 32],
    pub doc: ClassDoc,
}

/// A class file and the number of rendered targets that include it.
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct ClassUsage {
    pub name: String,
    pub file: PathBuf,
    pub targets: usize,
}

/// Everything a class contributes, including its own classes, memoised per
/// class file. Merging two closures is associative, so a class rendered once
/// serves every target that includes it.
pub struct ClassClosure {
    pub params: Node,
    pub classes: Vec<String>,
    pub applications: Vec<Node>,
    pub exports: Node,
    /// Every file this closure was built from (itself first).
    pub files: Vec<PathBuf>,
    /// Every path that was checked while resolving class names (hits and
    /// misses). Creating a file at one of these can change the render.
    pub probes: Vec<PathBuf>,
    pub log: Arc<Vec<MergeEvent>>,
}

/// Provenance of a rendered target.
#[derive(Default, Clone, Serialize)]
pub struct Provenance {
    /// Merge histories of the included classes, in inclusion order, followed by
    /// the target's own merge events.
    pub merges: Vec<Arc<Vec<MergeEvent>>>,
    pub resolutions: Vec<ResolveEvent>,
}

impl Provenance {
    /// All merge events touching `path` (or its children when `deep`).
    pub fn merge_events<'p>(
        &'p self,
        path: &'p KeyPath,
        deep: bool,
    ) -> impl Iterator<Item = &'p MergeEvent> + 'p {
        self.merges
            .iter()
            .flat_map(|log| log.iter())
            .filter(move |e| {
                let p = match e {
                    MergeEvent::Override { path, .. }
                    | MergeEvent::ListAppend { path, .. }
                    | MergeEvent::Dereference { path, .. } => path,
                };
                p == path || (deep && p.starts_with(path))
            })
    }

    pub fn resolution_of(&self, path: &KeyPath) -> Option<&ResolveEvent> {
        self.resolutions.iter().find(|e| &e.path == path)
    }
}

#[derive(Clone, Serialize)]
pub struct RenderedTarget {
    pub name: String,
    pub path: String,
    pub parameters: Node,
    pub classes: Vec<String>,
    pub applications: Vec<Node>,
    pub exports: Node,
    /// Files whose content determined this render (target file first).
    pub files: Vec<PathBuf>,
    /// Paths probed while resolving class names; see `ClassClosure::probes`.
    pub probes: Vec<PathBuf>,
    /// Content digest over `files`: identical digest means identical render.
    pub digest: String,
    /// Digest of the rendered document (`to_document()` as JSON): identical
    /// digest means identical compile input.
    pub doc_digest: String,
    #[serde(skip)]
    pub provenance: Provenance,
    pub warnings: Vec<Diagnostic>,
}

impl RenderedTarget {
    /// The reference `kapitan inventory -t` document shape.
    pub fn to_document(&self) -> Node {
        let mut m = crate::value::Map::new();
        m.insert("parameters".into(), self.parameters.clone());
        m.insert(
            "classes".into(),
            Node::synthetic(Value::List(
                self.classes
                    .iter()
                    .map(|c| Node::synthetic(Value::Str(c.clone())))
                    .collect(),
            )),
        );
        m.insert(
            "applications".into(),
            Node::synthetic(Value::List(self.applications.clone())),
        );
        m.insert("exports".into(), self.exports.clone());
        // kapitan 0.36 leaks this internal flag into its output; kept for byte compatibility.
        m.insert("resolved".into(), Node::synthetic(Value::Bool(false)));
        Node::synthetic(Value::Map(m))
    }
}

pub struct Inventory {
    pub cfg: InventoryConfig,
    pub sources: Sources,
    pub registry: Arc<Registry>,
    files: Mutex<HashMap<PathBuf, Arc<LoadedFile>>>,
    closures: Mutex<HashMap<PathBuf, Arc<ClassClosure>>>,
}

impl Inventory {
    pub fn new(cfg: InventoryConfig, registry: Arc<Registry>) -> Self {
        Inventory {
            cfg,
            sources: Sources::new(),
            registry,
            files: Mutex::new(HashMap::new()),
            closures: Mutex::new(HashMap::new()),
        }
    }

    pub fn open(root: impl Into<PathBuf>) -> Self {
        Self::new(
            InventoryConfig::new(root),
            Arc::new(Registry::with_builtins()),
        )
    }

    /// Forget everything cached about `path` (and every closure built from it).
    pub fn invalidate(&self, path: &Path) -> Vec<PathBuf> {
        self.files.lock().remove(path);
        let mut closures = self.closures.lock();
        let dropped: Vec<PathBuf> = closures
            .iter()
            .filter(|(_, c)| c.files.iter().chain(c.probes.iter()).any(|f| f == path))
            .map(|(k, _)| k.clone())
            .collect();
        for k in &dropped {
            closures.remove(k);
        }
        dropped
    }

    /// Drop every cached closure that (transitively) depends on `paths`, by
    /// content or by class resolution. `files`/`probes` already list the
    /// transitive inputs, so one pass over the cache is enough.
    pub fn invalidate_dependents(&self, paths: &[PathBuf]) {
        let mut closures = self.closures.lock();
        closures.retain(|_, c| {
            !c.files
                .iter()
                .chain(c.probes.iter())
                .any(|f| paths.contains(f))
        });
        let mut files = self.files.lock();
        for p in paths {
            files.remove(p);
        }
    }

    /// Drop every cached closure that depends on anything under `dir`.
    pub fn invalidate_under(&self, dir: &Path) {
        let mut closures = self.closures.lock();
        closures.retain(|_, c| {
            !c.files
                .iter()
                .chain(c.probes.iter())
                .any(|f| f.starts_with(dir))
        });
        self.files.lock().retain(|p, _| !p.starts_with(dir));
    }

    pub fn invalidate_all(&self) {
        self.files.lock().clear();
        self.closures.lock().clear();
    }

    /// Class files currently cached, with the files each depends on.
    pub fn cached_closures(&self) -> Vec<(PathBuf, Vec<PathBuf>)> {
        self.closures
            .lock()
            .iter()
            .map(|(k, c)| (k.clone(), c.files.clone()))
            .collect()
    }

    // ---- discovery --------------------------------------------------------

    pub fn discover_targets(&self) -> Result<Vec<TargetSpec>> {
        let dir = self.cfg.targets_dir();
        if !dir.is_dir() {
            return Err(Error::new(
                "inventory::no_targets_dir",
                format!("no targets directory at {}", dir.display()),
            )
            .with_help(
                "run from the directory containing `inventory/`, or pass --inventory-path",
            ));
        }
        let mut files = Vec::new();
        walk(&dir, &mut files)?;
        files.sort();
        let mut targets: Vec<TargetSpec> = Vec::new();
        let mut seen: HashMap<String, &TargetSpec> = HashMap::new();
        let mut specs = Vec::new();
        for file in files {
            let ext = file.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yml" && ext != "yaml" {
                continue;
            }
            let rel = file.strip_prefix(&dir).unwrap();
            let rel_str = rel
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            let stem = rel_str
                .rsplit_once('.')
                .map(|(s, _)| s.to_string())
                .unwrap_or(rel_str.clone());
            let name = if self.cfg.compose_target_name {
                stem.replace('/', ".")
            } else {
                file.file_stem().unwrap().to_string_lossy().to_string()
            };
            specs.push(TargetSpec {
                name,
                path: rel_str,
                file,
            });
        }
        for spec in &specs {
            if let Some(other) = seen.get(&spec.name) {
                return Err(Error::new(
                    "inventory::conflicting_targets",
                    format!(
                        "conflicting targets {}: {} and {}",
                        spec.name, spec.path, other.path
                    ),
                )
                .with_help("enable compose-target-name so nested targets get distinct names"));
            }
            seen.insert(spec.name.clone(), spec);
        }
        targets.extend(specs);
        Ok(targets)
    }

    pub fn target_spec(&self, name: &str) -> Result<TargetSpec> {
        let targets = self.discover_targets()?;
        targets
            .into_iter()
            .find(|t| t.name == name || t.dotted_path() == name)
            .ok_or_else(|| {
                Error::new(
                    "inventory::unknown_target",
                    format!("target `{name}` not found"),
                )
                .with_help("list targets with `krab inventory targets`")
            })
    }

    /// Every class file under `classes/`, with its dotted class name.
    pub fn class_files(&self) -> Result<Vec<(String, PathBuf)>> {
        let dir = self.cfg.classes_dir();
        let mut files = Vec::new();
        if dir.is_dir() {
            walk(&dir, &mut files)?;
        }
        files.sort();
        Ok(files
            .into_iter()
            .filter(|f| matches!(f.extension().and_then(|e| e.to_str()), Some("yml" | "yaml")))
            .map(|f| {
                let rel = f.strip_prefix(&dir).unwrap().with_extension("");
                let mut name = rel
                    .to_string_lossy()
                    .replace(std::path::MAIN_SEPARATOR, ".");
                if let Some(stripped) = name.strip_suffix(".init") {
                    name = stripped.to_string();
                } else if name == "init" {
                    name = String::new();
                }
                (name, f)
            })
            .collect())
    }

    /// How many rendered targets each class file ends up in. Classes with a
    /// zero count are dead: nothing includes them.
    pub fn class_usage(&self, report: &RenderReport) -> Result<Vec<ClassUsage>> {
        let mut usage: Vec<ClassUsage> = self
            .class_files()?
            .into_iter()
            .map(|(name, file)| ClassUsage {
                name,
                file,
                targets: 0,
            })
            .collect();
        let index: HashMap<&PathBuf, usize> = usage
            .iter()
            .enumerate()
            .map(|(i, u)| (&u.file, i))
            .collect();
        let mut counts = vec![0usize; usage.len()];
        for t in report.targets.values() {
            for f in t.files.iter().skip(1) {
                if let Some(&i) = index.get(f) {
                    counts[i] += 1;
                }
            }
        }
        for (u, c) in usage.iter_mut().zip(counts) {
            u.targets = c;
        }
        Ok(usage)
    }

    // ---- files & classes --------------------------------------------------

    fn load(&self, path: &Path) -> Result<Arc<LoadedFile>> {
        if let Some(f) = self.files.lock().get(path) {
            return Ok(f.clone());
        }
        let text = std::fs::read_to_string(path)
            .map_err(|e| Error::new("io", format!("cannot read {}: {e}", path.display())))?;
        let digest = *blake3::hash(text.as_bytes()).as_bytes();
        let id = self.sources.intern(path);
        let node = yaml::parse_document(&text, id)?;
        let doc = ClassDoc::from_node(node)?;
        let loaded = Arc::new(LoadedFile {
            path: path.to_path_buf(),
            digest,
            doc,
        });
        self.files.lock().insert(path.to_path_buf(), loaded.clone());
        Ok(loaded)
    }

    /// Resolve a class name to a file, the way the reference does: `a.b` is
    /// `classes/a/b/init.yml` or `classes/a/b.yml`; `.a` is relative to the
    /// including class' directory; two reclass compatibility fallbacks drop
    /// the first two name components.
    pub fn resolve_class_file(&self, class_name: &str, from_file: &Path) -> ClassResolution {
        let classes = self.cfg.classes_dir();
        let parent_dir: PathBuf = match from_file
            .parent()
            .and_then(|p| p.strip_prefix(&classes).ok())
        {
            Some(rel) => rel.to_path_buf(),
            None => from_file
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default(),
        };
        let base = if class_name.starts_with('.') {
            classes.join(&parent_dir)
        } else {
            classes.clone()
        };
        let parts: Vec<&str> = class_name.split('.').collect();
        let joined = |segments: &[&str]| -> PathBuf {
            let mut p = base.clone();
            for s in segments {
                if !s.is_empty() {
                    p.push(s);
                }
            }
            p
        };
        let mut tried = Vec::new();
        for ext in ["yml", "yaml"] {
            let full = joined(&parts);
            let compat = joined(parts.get(2..).unwrap_or(&[]));
            let cases = [
                full.join(format!("init.{ext}")),
                with_ext(&full, ext),
                with_ext(&compat, ext),
                compat.join(format!("init.{ext}")),
            ];
            for case in cases {
                let hit = case.is_file();
                tried.push(case.clone());
                if hit {
                    return ClassResolution {
                        found: Some(case),
                        tried,
                    };
                }
            }
        }
        ClassResolution { found: None, tried }
    }

    fn class_closure(&self, file: &Path, stack: &mut Vec<PathBuf>) -> Result<Arc<ClassClosure>> {
        if let Some(c) = self.closures.lock().get(file) {
            return Ok(c.clone());
        }
        let closure = Arc::new(self.build_closure(file, None, stack)?);
        self.closures
            .lock()
            .insert(file.to_path_buf(), closure.clone());
        Ok(closure)
    }

    /// The recursive loader: classes first (depth first, in order), then the
    /// file's own parameters.
    fn build_closure(
        &self,
        file: &Path,
        initial: Option<Node>,
        stack: &mut Vec<PathBuf>,
    ) -> Result<ClassClosure> {
        let loaded = self.load(file)?;
        stack.push(file.to_path_buf());
        let result = self.build_closure_inner(file, &loaded.doc, initial, stack);
        stack.pop();
        result
    }

    fn build_closure_inner(
        &self,
        file: &Path,
        doc: &ClassDoc,
        initial: Option<Node>,
        stack: &mut Vec<PathBuf>,
    ) -> Result<ClassClosure> {
        let mut params = initial.unwrap_or_else(|| Node::map(Origin::SYNTHETIC));
        let mut classes = Vec::new();
        let mut applications = Vec::new();
        let mut exports = Node::map(Origin::SYNTHETIC);
        let mut files = vec![file.to_path_buf()];
        let mut probes: Vec<PathBuf> = Vec::new();
        let mut own_log: Vec<MergeEvent> = Vec::new();
        let mut log = if self.cfg.track_provenance {
            Some(&mut own_log)
        } else {
            None
        };
        let mut nested_logs: Vec<Arc<Vec<MergeEvent>>> = Vec::new();
        let deref = EvalDeref { inv: self };
        for class_ref in &doc.classes {
            let resolution = self.resolve_class_file(&class_ref.name, file);
            for p in &resolution.tried {
                if !probes.contains(p) {
                    probes.push(p.clone());
                }
            }
            let class_file = match resolution.found {
                Some(p) => p,
                None => {
                    let tried = resolution.tried;
                    if self.cfg.ignore_class_not_found {
                        continue;
                    }
                    return Err(Error::new(
                        "inventory::class_not_found",
                        format!("class `{}` not found", class_ref.name),
                    )
                    .with_label(class_ref.origin, "referenced here")
                    .with_help(format!(
                        "looked for:\n{}",
                        tried
                            .iter()
                            .take(4)
                            .map(|p| format!("  {}", p.display()))
                            .collect::<Vec<_>>()
                            .join("\n")
                    )));
                }
            };
            if stack.contains(&class_file) {
                // The reference recurses forever here; we stop with a clear error.
                return Err(Error::new(
                    "inventory::class_cycle",
                    format!(
                        "class `{}` includes itself (directly or through its parents)",
                        class_ref.name
                    ),
                )
                .with_label(class_ref.origin, "cycle closes here"));
            }
            let closure = self.class_closure(&class_file, stack).map_err(|e| {
                e.with_label(
                    class_ref.origin,
                    format!("included as `{}`", class_ref.name),
                )
            })?;
            if !closure.params.as_map().is_some_and(|m| m.is_empty()) {
                merge(&mut params, closure.params.clone(), &deref, &mut log);
            }
            classes.extend(closure.classes.iter().cloned());
            classes.push(class_ref.name.clone());
            applications.extend(closure.applications.iter().cloned());
            merge(&mut exports, closure.exports.clone(), &deref, &mut None);
            for f in &closure.files {
                if !files.contains(f) {
                    files.push(f.clone());
                }
            }
            for p in &closure.probes {
                if !probes.contains(p) {
                    probes.push(p.clone());
                }
            }
            nested_logs.push(closure.log.clone());
        }
        if !doc.parameters.as_map().is_some_and(|m| m.is_empty()) {
            merge(&mut params, doc.parameters.clone(), &deref, &mut log);
        }
        merge(&mut exports, doc.exports.clone(), &deref, &mut None);
        applications.extend(doc.applications.iter().cloned());
        // Flatten: nested class logs in order, then this file's own events.
        let mut combined: Vec<MergeEvent> = Vec::new();
        for l in nested_logs {
            combined.extend(l.iter().cloned());
        }
        combined.extend(own_log);
        Ok(ClassClosure {
            params,
            classes,
            applications,
            exports,
            files,
            probes,
            log: Arc::new(combined),
        })
    }

    // ---- rendering --------------------------------------------------------

    pub fn render(&self, spec: &TargetSpec) -> Result<RenderedTarget> {
        let path_no_ext = spec
            .path
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or(spec.path.clone());
        let initial = model::initial_parameters(&spec.name, &path_no_ext);
        let closure = self
            .build_closure(&spec.file, Some(initial), &mut Vec::new())
            .map_err(|e| e.with_target(&spec.name))?;
        let ClassClosure {
            mut params,
            classes,
            applications,
            exports,
            files,
            probes,
            log,
        } = closure;

        let mut warnings = Vec::new();
        let resolutions = {
            let mut ev = Evaluator::new(
                &mut params,
                &self.registry,
                &self.sources,
                &spec.name,
                self.cfg.track_provenance,
            );
            ev.resolve_all(self.cfg.passes)?;
            warnings.append(&mut ev.warnings);
            std::mem::take(&mut ev.events)
        };
        process_literals(&mut params);
        if self.cfg.normalize {
            model::normalize(&mut params, &spec.name)?;
        }
        let mut hasher = blake3::Hasher::new();
        for f in &files {
            hasher.update(f.to_string_lossy().as_bytes());
            if let Some(loaded) = self.files.lock().get(f) {
                hasher.update(&loaded.digest);
            }
        }
        let digest = hasher.finalize().to_hex().to_string();
        let mut target = RenderedTarget {
            name: spec.name.clone(),
            path: spec.path.clone(),
            parameters: params,
            classes,
            applications,
            exports,
            files,
            probes,
            digest,
            doc_digest: String::new(),
            provenance: Provenance {
                merges: vec![log],
                resolutions,
            },
            warnings,
        };
        target.doc_digest = blake3::hash(&serde_json::to_vec(&target.to_document()).unwrap())
            .to_hex()
            .to_string();
        Ok(target)
    }

    pub fn render_named(&self, name: &str) -> Result<RenderedTarget> {
        let spec = self.target_spec(name)?;
        self.render(&spec)
    }

    /// Render every target in parallel. Failures are collected per target.
    pub fn render_all(&self) -> Result<RenderReport> {
        let specs = self.discover_targets()?;
        self.render_many(&specs)
    }

    pub fn render_many(&self, specs: &[TargetSpec]) -> Result<RenderReport> {
        let results: Vec<(String, Result<RenderedTarget>)> = specs
            .par_iter()
            .map(|s| (s.name.clone(), self.render(s)))
            .collect();
        let mut report = RenderReport::default();
        for (name, r) in results {
            match r {
                Ok(t) => {
                    report.targets.insert(name, t);
                }
                Err(e) => report
                    .errors
                    .push(e.with_target(name).resolve(&self.sources)),
            }
        }
        Ok(report)
    }
}

#[derive(Default)]
pub struct RenderReport {
    pub targets: std::collections::BTreeMap<String, RenderedTarget>,
    pub errors: Vec<Error>,
}

/// Merge-time dereferencing: evaluate `${...}` against a snapshot of the tree
/// merged so far. Only used when a container is merged over a placeholder.
struct EvalDeref<'i> {
    inv: &'i Inventory,
}

impl MergeDeref for EvalDeref<'_> {
    fn deref(&self, root: &Node, at: &KeyPath, _expr: &str) -> Option<Value> {
        let mut snapshot = root.clone();
        let mut ev = Evaluator::new(
            &mut snapshot,
            &self.inv.registry,
            &self.inv.sources,
            "",
            false,
        );
        let r = ev.deref_at(at).ok()?;
        ev.deep_value(r).ok()
    }
}

fn with_ext(p: &Path, ext: &str) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".");
    s.push(ext);
    PathBuf::from(s)
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(dir)
        .map_err(|e| Error::new("io", format!("cannot read {}: {e}", dir.display())))?
    {
        let entry = entry.map_err(|e| Error::new("io", e.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway inventory tree. No test-only dependency for this: the
    /// directory is unique per test and removed when the guard drops.
    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str, files: &[&str]) -> Tree {
            let root =
                std::env::temp_dir().join(format!("krab-targets-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for f in files {
                let path = root.join("targets").join(f);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "parameters:\n  a: 1\n").unwrap();
            }
            std::fs::create_dir_all(root.join("classes")).unwrap();
            Tree(root)
        }

        fn inventory(&self, compose: bool) -> Inventory {
            let mut cfg = InventoryConfig::new(&self.0);
            cfg.compose_target_name = compose;
            Inventory::new(cfg, Arc::new(Registry::with_builtins()))
        }

        fn names(&self, compose: bool) -> Vec<String> {
            let mut n: Vec<String> = self
                .inventory(compose)
                .discover_targets()
                .unwrap()
                .into_iter()
                .map(|t| t.name)
                .collect();
            n.sort();
            n
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_target_in_a_subdirectory_is_named_after_its_file() {
        let tree = Tree::new("subdir", &["prod/app.yml", "top.yml"]);
        assert_eq!(tree.names(false), ["app", "top"]);
        assert_eq!(tree.names(true), ["prod.app", "top"]);
    }

    #[test]
    fn a_target_is_selectable_by_name_and_by_its_dotted_path() {
        let tree = Tree::new("select", &["prod/app.yml"]);
        let inv = tree.inventory(false);
        assert_eq!(inv.target_spec("app").unwrap().name, "app");
        assert_eq!(inv.target_spec("prod.app").unwrap().name, "app");
        assert!(inv.target_spec("nope").is_err());
    }

    #[test]
    fn two_files_with_one_name_are_reported_rather_than_rendered() {
        let tree = Tree::new("conflict", &["a/x.yml", "b/x.yml"]);
        let err = tree.inventory(false).discover_targets().unwrap_err();
        assert_eq!(err.diagnostic().code, "inventory::conflicting_targets");
    }
}

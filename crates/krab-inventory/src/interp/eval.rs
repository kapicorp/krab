//! Evaluation of interpolations over a parameter tree, with OmegaConf's
//! `resolve()` semantics:
//!
//! * nodes are visited in order; each `${...}` string is evaluated and written
//!   back;
//! * a node interpolation that points at a container resolves that container
//!   in place first and then copies it;
//! * resolver results are written back verbatim, so a resolver returning a
//!   string that contains `${` gets evaluated again on the next pass;
//! * cycles and references to an enclosing container are errors.

use std::collections::HashMap;
use std::rc::Rc;

use crate::error::{Diagnostic, Error, Result};
use crate::interp::ast::*;
use crate::interp::parse::parse_text;
use crate::merge::{get, get_mut};
use crate::path::{Key, KeyPath, split_key};
use crate::resolvers::{Ctx, Registry};
use crate::source::{Origin, Sources};
use crate::value::{Map, Node, Value};

/// Outcome of evaluating an interpolation: either an alias to an existing
/// node in the tree, or a freshly computed value.
#[derive(Clone, Debug)]
pub enum Resolved {
    At(KeyPath),
    Owned(Value),
}

/// Provenance record: how a `${...}` at `path` was resolved.
#[derive(Clone, Debug, serde::Serialize)]
pub struct ResolveEvent {
    pub path: KeyPath,
    pub expr: String,
    /// The node the expression aliased, when it was a plain node interpolation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<KeyPath>,
    #[serde(skip)]
    pub origin: Origin,
}

/// How many copies of copies [`Evaluator::anchored`] follows.
const ANCHOR_DEPTH: usize = 8;

pub struct Evaluator<'a> {
    pub(crate) root: &'a mut Node,
    pub(crate) registry: &'a Registry,
    pub(crate) sources: &'a Sources,
    pub(crate) target: &'a str,
    asts: HashMap<String, Rc<Text>>,
    cache: HashMap<KeyPath, Resolved>,
    /// For a value copied out of another container by an interpolation: where
    /// the container came from. See [`Evaluator::anchored`].
    anchors: HashMap<KeyPath, KeyPath>,
    /// The first container source seen while the current node is evaluated.
    pending_source: Option<KeyPath>,
    stack: Vec<KeyPath>,
    track: bool,
    pub events: Vec<ResolveEvent>,
    pub warnings: Vec<Diagnostic>,
}

impl<'a> Evaluator<'a> {
    pub fn new(
        root: &'a mut Node,
        registry: &'a Registry,
        sources: &'a Sources,
        target: &'a str,
        track: bool,
    ) -> Self {
        Evaluator {
            root,
            registry,
            sources,
            target,
            asts: HashMap::new(),
            cache: HashMap::new(),
            anchors: HashMap::new(),
            pending_source: None,
            stack: Vec::new(),
            track,
            events: Vec::new(),
            warnings: Vec::new(),
        }
    }

    pub fn root(&self) -> &Node {
        self.root
    }

    /// Run `passes` full resolution passes over the tree.
    pub fn resolve_all(&mut self, passes: usize) -> Result<()> {
        for _ in 0..passes {
            self.cache.clear();
            self.resolve_subtree(&KeyPath::root())?;
        }
        Ok(())
    }

    fn resolve_subtree(&mut self, path: &KeyPath) -> Result<()> {
        enum Kind {
            Interp,
            Map(Vec<String>),
            List(usize),
            Other,
        }
        let kind = match get(self.root, path).map(|n| &n.value) {
            Some(Value::Str(s)) if s.contains("${") => Kind::Interp,
            Some(Value::Map(m)) => Kind::Map(m.keys().cloned().collect()),
            Some(Value::List(l)) => Kind::List(l.len()),
            _ => Kind::Other,
        };
        match kind {
            Kind::Interp => self.resolve_node_at(path),
            Kind::Map(keys) => {
                for k in keys {
                    let child = path.child(Key::Str(k));
                    if get(self.root, &child).is_some() {
                        self.resolve_subtree(&child)?;
                    }
                }
                Ok(())
            }
            Kind::List(n) => {
                for i in 0..n {
                    let child = path.child(Key::Index(i));
                    if get(self.root, &child).is_some() {
                        self.resolve_subtree(&child)?;
                    }
                }
                Ok(())
            }
            Kind::Other => Ok(()),
        }
    }

    fn resolve_node_at(&mut self, path: &KeyPath) -> Result<()> {
        let (expr, origin) = {
            let n = get(self.root, path).unwrap();
            (n.as_str().unwrap_or_default().to_string(), n.origin)
        };
        let outer_source = self.pending_source.take();
        let resolved = self.deref_at(path)?;
        let source = match resolved {
            Resolved::At(q) => {
                self.resolve_subtree(&q)?;
                let value = get(self.root, &q).unwrap().value.clone();
                *get_mut(self.root, path).unwrap() = Node::new(value, origin);
                Some(q)
            }
            Resolved::Owned(value) => {
                *get_mut(self.root, path).unwrap() = Node::new(value, origin);
                None
            }
        };
        if let Some(from) = self.pending_source.take()
            && get(self.root, path).is_some_and(|n| n.value.is_container())
        {
            self.anchors.insert(path.clone(), from);
        }
        self.pending_source = outer_source;
        if self.track {
            self.events.push(ResolveEvent {
                path: path.clone(),
                expr,
                source,
                origin,
            });
        }
        Ok(())
    }

    /// Where the value at `path` was written, following the copies that
    /// brought it here.
    ///
    /// A container reached through an interpolation - an alias, a
    /// `${merge:...}` argument - keeps reporting the place it came from, so
    /// `${key:}`, `${parentkey:}` and `${fullkey:}` written in a class and
    /// copied into a component still name the class's key. The reference gets
    /// this from the node metadata that `OmegaConf.merge` carries over from
    /// its first argument; krab copies values rather than nodes, so the origin
    /// is kept here instead.
    ///
    /// Only those three resolvers consult it. An ordinary interpolation, and a
    /// relative one produced by `${relpath:...}`, is resolved by walking the
    /// tree from where the value now sits, in both implementations.
    pub(crate) fn anchored(&self, path: &KeyPath) -> KeyPath {
        let mut path = path.clone();
        for _ in 0..ANCHOR_DEPTH {
            let Some((prefix, from)) = self
                .anchors
                .iter()
                .filter(|(dest, _)| path.starts_with(dest))
                .max_by_key(|(dest, _)| dest.0.len())
            else {
                return path;
            };
            let mut next = from.clone();
            next.0.extend_from_slice(&path.0[prefix.0.len()..]);
            if next == path {
                return path;
            }
            path = next;
        }
        path
    }

    fn parse(&mut self, expr: &str, origin: Origin, path: &KeyPath) -> Result<Rc<Text>> {
        if let Some(t) = self.asts.get(expr) {
            return Ok(t.clone());
        }
        let text = parse_text(expr).map_err(|e| {
            Error::new(
                "interpolation::syntax",
                format!("invalid interpolation {expr:?}: {}", e.message),
            )
            .with_target(self.target)
            .with_path(path.to_string())
            .with_label(origin, "in this value")
        })?;
        let rc = Rc::new(text);
        self.asts.insert(expr.to_string(), rc.clone());
        Ok(rc)
    }

    /// Evaluate the interpolation string stored at `path` (memoised per pass).
    pub(crate) fn deref_at(&mut self, path: &KeyPath) -> Result<Resolved> {
        if let Some(r) = self.cache.get(path) {
            return Ok(r.clone());
        }
        let (expr, origin) = {
            let n = get(self.root, path).unwrap();
            (n.as_str().unwrap_or_default().to_string(), n.origin)
        };
        if self.stack.contains(path) {
            let mut err = Error::new(
                "interpolation::recursive",
                format!("recursive interpolation detected while resolving {expr:?}"),
            )
            .with_target(self.target)
            .with_path(path.to_string())
            .with_label(origin, "this value refers back to itself");
            for p in self.stack.iter().rev() {
                if let Some(n) = get(self.root, p) {
                    err = err.with_label(n.origin, format!("via {p}"));
                }
            }
            return Err(err);
        }
        let ast = self.parse(&expr, origin, path)?;
        self.stack.push(path.clone());
        let result = self.eval_text(&ast, path, origin);
        self.stack.pop();
        let result = result?;
        self.cache.insert(path.clone(), result.clone());
        Ok(result)
    }

    pub(crate) fn eval_text(
        &mut self,
        text: &Text,
        at: &KeyPath,
        origin: Origin,
    ) -> Result<Resolved> {
        if let Some(i) = text.single_interp() {
            return self.eval_interp(i, at, origin);
        }
        let mut out = String::new();
        for part in &text.0 {
            match part {
                TextPart::Lit(s) => out.push_str(s),
                TextPart::Interp(i) => {
                    let r = self.eval_interp(i, at, origin)?;
                    out.push_str(&self.py_str(&r));
                }
            }
        }
        Ok(Resolved::Owned(Value::Str(out)))
    }

    pub(crate) fn py_str(&self, r: &Resolved) -> String {
        match r {
            Resolved::At(q) => get(self.root, q)
                .map(|n| n.value.py_str())
                .unwrap_or_default(),
            Resolved::Owned(v) => v.py_str(),
        }
    }

    pub(crate) fn to_value(&self, r: Resolved) -> Value {
        match r {
            Resolved::At(q) => get(self.root, &q)
                .map(|n| n.value.clone())
                .unwrap_or(Value::Null),
            Resolved::Owned(v) => v,
        }
    }

    fn eval_interp(&mut self, interp: &Interp, at: &KeyPath, origin: Origin) -> Result<Resolved> {
        match interp {
            Interp::Node { dots, keys } => {
                let mut segs = Vec::with_capacity(keys.len());
                for k in keys {
                    match k {
                        KeySeg::Lit(s) => segs.push(s.clone()),
                        KeySeg::Interp(i) => {
                            let r = self.eval_interp(i, at, origin)?;
                            match self.to_value(r) {
                                Value::Str(s) => segs.push(s),
                                Value::Int(i) => segs.push(i.to_string()),
                                other => {
                                    return Err(self
                                        .err("interpolation::bad_key", format!(
                                            "interpolation used as a key must resolve to a string or integer, got {} ({})",
                                            other.py_repr(),
                                            other.type_name()
                                        ), at, origin));
                                }
                            }
                        }
                    }
                }
                let base = self.relative_base(*dots, at, origin)?;
                let full = format!("{}{}", ".".repeat(*dots), segs.join("."));
                match self.select_path(base, &segs, at, origin)? {
                    Some(r) => {
                        if let Resolved::At(q) = &r
                            && self.pending_source.is_none()
                            && get(self.root, q).is_some_and(|n| n.value.is_container())
                        {
                            self.pending_source = Some(q.clone());
                        }
                        if let Resolved::At(q) = &r
                            && let Some(parent) = at.parent()
                            && parent.starts_with(q)
                        {
                            return Err(self.err(
                                "interpolation::parent_reference",
                                format!("interpolation {full:?} points at an enclosing container"),
                                at,
                                origin,
                            ));
                        }
                        Ok(r)
                    }
                    None => Err(self
                        .err("interpolation::key_not_found", format!("interpolation key '{full}' not found"), at, origin)
                        .with_help("check the spelling, or provide a default with ${oc.select:key,default}")),
                }
            }
            Interp::Resolver { name, args } => {
                let mut parts = Vec::with_capacity(name.len());
                for n in name {
                    match n {
                        NamePart::Lit(s) => parts.push(s.clone()),
                        NamePart::Interp(i) => {
                            let r = self.eval_interp(i, at, origin)?;
                            match self.to_value(r) {
                                Value::Str(s) => parts.push(s),
                                other => {
                                    return Err(self.err(
                                        "interpolation::bad_resolver_name",
                                        format!(
                                            "resolver name must be a string, got {}",
                                            other.type_name()
                                        ),
                                        at,
                                        origin,
                                    ));
                                }
                            }
                        }
                    }
                }
                let name = parts.join(".");
                let mut values = Vec::with_capacity(args.len());
                let mut kinds = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.eval_element(a, at, origin)?);
                    kinds.push(match a {
                        Element::Prim(Prim::Interp(i)) => match **i {
                            Interp::Node { .. } => crate::resolvers::ArgKind::Node,
                            Interp::Resolver { .. } => crate::resolvers::ArgKind::Computed,
                        },
                        Element::Prim(Prim::Concat(_)) => crate::resolvers::ArgKind::Computed,
                        _ => crate::resolvers::ArgKind::Literal,
                    });
                }
                let Some(resolver) = self.registry.get(&name) else {
                    return Err(self
                        .err(
                            "interpolation::unknown_resolver",
                            format!("unsupported interpolation type `{name}`"),
                            at,
                            origin,
                        )
                        .with_help(match self.registry.description() {
                            "" => format!("known resolvers: {}", self.registry.names().join(", ")),
                            from => format!(
                                "known resolvers: {}; {from}",
                                self.registry.names().join(", ")
                            ),
                        }));
                };
                let mut ctx = Ctx {
                    ev: self,
                    at: at.clone(),
                    origin,
                    resolver: &name,
                    arg_kinds: kinds,
                };
                match resolver(&mut ctx, &values) {
                    Ok(v) => Ok(Resolved::Owned(v)),
                    Err(e) => match e {
                        crate::resolvers::ResolverError::Message(msg) => Err(self.err(
                            "interpolation::resolver_failed",
                            format!("resolver `{name}` failed: {msg}"),
                            at,
                            origin,
                        )),
                        crate::resolvers::ResolverError::Inner(err) => Err(err),
                    },
                }
            }
        }
    }

    fn relative_base(&self, dots: usize, at: &KeyPath, origin: Origin) -> Result<KeyPath> {
        if dots == 0 {
            return Ok(KeyPath::root());
        }
        let mut base = at.clone();
        for _ in 0..dots {
            base = base.parent().ok_or_else(|| {
                self.err(
                    "interpolation::bad_relative",
                    "relative interpolation goes above the root",
                    at,
                    origin,
                )
            })?;
        }
        Ok(base)
    }

    /// OmegaConf `_select_impl`: walk `keys` from `base`, dereferencing
    /// interpolations met on the way. `Ok(None)` means "not found".
    pub(crate) fn select_path(
        &mut self,
        base: KeyPath,
        keys: &[String],
        at: &KeyPath,
        origin: Origin,
    ) -> Result<Option<Resolved>> {
        let mut cur = Resolved::At(base);
        for (i, key) in keys.iter().enumerate() {
            cur = match cur {
                Resolved::At(p) => {
                    let node = get(self.root, &p).unwrap();
                    let child = match &node.value {
                        Value::Map(m) => {
                            if m.contains_key(key) {
                                Some(Key::Str(key.clone()))
                            } else {
                                None
                            }
                        }
                        Value::List(l) => match key.trim().parse::<i64>() {
                            Ok(mut idx) => {
                                if idx < 0 {
                                    idx += l.len() as i64;
                                }
                                if idx < 0 || idx as usize >= l.len() {
                                    None
                                } else {
                                    Some(Key::Index(idx as usize))
                                }
                            }
                            Err(_) => {
                                return Err(self.err(
                                    "interpolation::bad_index",
                                    format!("index '{key}' (str) is not an int"),
                                    at,
                                    origin,
                                ));
                            }
                        },
                        Value::Null if i > 0 => None,
                        other => {
                            return Err(self.err(
                                "interpolation::not_a_container",
                                format!(
                                    "error trying to access {}: node `{p}` is not a container ({}) and thus cannot contain `{key}`",
                                    keys.join("."),
                                    other.type_name()
                                ),
                                at,
                                origin,
                            ));
                        }
                    };
                    let Some(child) = child else { return Ok(None) };
                    let cp = p.child(child);
                    if get(self.root, &cp).unwrap().is_interpolation() {
                        self.deref_at(&cp)?
                    } else {
                        Resolved::At(cp)
                    }
                }
                Resolved::Owned(v) => {
                    let child = match &v {
                        Value::Map(m) => m.get(key).map(|n| n.value.clone()),
                        Value::List(l) => key.trim().parse::<i64>().ok().and_then(|mut idx| {
                            if idx < 0 {
                                idx += l.len() as i64;
                            }
                            l.get(usize::try_from(idx).ok()?).map(|n| n.value.clone())
                        }),
                        _ => None,
                    };
                    match child {
                        Some(c) => Resolved::Owned(c),
                        None => return Ok(None),
                    }
                }
            };
        }
        Ok(Some(cur))
    }

    fn eval_element(&mut self, el: &Element, at: &KeyPath, origin: Origin) -> Result<Value> {
        Ok(match el {
            Element::Prim(p) => self.eval_prim(p, at, origin)?,
            Element::Quoted(text) => {
                let r = self.eval_text(text, at, origin)?;
                Value::Str(self.py_str(&r))
            }
            Element::List(items) => {
                let mut out = Vec::with_capacity(items.len());
                for i in items {
                    out.push(Node::new(self.eval_element(i, at, origin)?, origin));
                }
                Value::List(out)
            }
            Element::Dict(pairs) => {
                let mut out = Map::with_capacity(pairs.len());
                for (k, v) in pairs {
                    let key = self.eval_prim(k, at, origin)?.py_str();
                    let value = self.eval_element(v, at, origin)?;
                    out.insert(key, Node::new(value, origin));
                }
                Value::Map(out)
            }
        })
    }

    fn eval_prim(&mut self, p: &Prim, at: &KeyPath, origin: Origin) -> Result<Value> {
        Ok(match p {
            Prim::Null => Value::Null,
            Prim::Bool(b) => Value::Bool(*b),
            Prim::Int(i) => Value::Int(*i),
            Prim::Float(f) => Value::Float(*f),
            Prim::Str(s) => Value::Str(s.clone()),
            Prim::Interp(i) => {
                let r = self.eval_interp(i, at, origin)?;
                self.to_value(r)
            }
            Prim::Concat(parts) => {
                let r = self.eval_text(&Text(parts.clone()), at, origin)?;
                Value::Str(self.py_str(&r))
            }
        })
    }

    /// Fully resolve `r` into an owned value, evaluating any interpolation
    /// strings inside (OmegaConf `to_container(resolve=True)`), without
    /// writing back.
    pub(crate) fn deep_value(&mut self, r: Resolved) -> Result<Value> {
        match r {
            Resolved::Owned(v) => Ok(v),
            Resolved::At(q) => {
                let v = get(self.root, &q).unwrap().value.clone();
                self.deep_resolve_value(v, &q)
            }
        }
    }

    fn deep_resolve_value(&mut self, v: Value, at: &KeyPath) -> Result<Value> {
        match v {
            Value::Str(s) if s.contains("${") => {
                let r = self.deref_at(at)?;
                self.deep_value(r)
            }
            Value::Map(m) => {
                let mut out = Map::with_capacity(m.len());
                for (k, node) in m {
                    let child = at.child(Key::Str(k.clone()));
                    let value = self.deep_resolve_value(node.value, &child)?;
                    out.insert(k, Node::new(value, node.origin));
                }
                Ok(Value::Map(out))
            }
            Value::List(l) => {
                let mut out = Vec::with_capacity(l.len());
                for (i, node) in l.into_iter().enumerate() {
                    let child = at.child(Key::Index(i));
                    let value = self.deep_resolve_value(node.value, &child)?;
                    out.push(Node::new(value, node.origin));
                }
                Ok(Value::List(out))
            }
            other => Ok(other),
        }
    }

    /// `OmegaConf.select(root, key)` style lookup used by resolvers: absolute
    /// unless `key` starts with `.`, in which case it is relative to `base`.
    pub(crate) fn select_key(
        &mut self,
        base: &KeyPath,
        key: &str,
        at: &KeyPath,
        origin: Origin,
    ) -> Result<Option<Resolved>> {
        let dots = key.chars().take_while(|c| *c == '.').count();
        let rest = &key[dots..];
        let start = if dots == 0 {
            KeyPath::root()
        } else {
            let mut b = base.clone();
            for _ in 1..dots {
                b = b.parent().ok_or_else(|| {
                    self.err(
                        "interpolation::bad_relative",
                        "relative key goes above the root",
                        at,
                        origin,
                    )
                })?;
            }
            b
        };
        if rest.is_empty() {
            return Ok(Some(Resolved::At(start)));
        }
        let keys = split_key(rest);
        match self.select_path(start, &keys, at, origin) {
            Ok(r) => Ok(r),
            Err(e) if e.diagnostic().code == "interpolation::not_a_container" => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn err(
        &self,
        code: &'static str,
        message: impl Into<String>,
        at: &KeyPath,
        origin: Origin,
    ) -> Error {
        Error::new(code, message)
            .with_target(self.target)
            .with_path(at.to_string())
            .with_label(origin, "in this value")
    }
}

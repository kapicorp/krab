//! Resolvers: the functions behind `${name:args}`.
//!
//! A resolver is any `Fn(&mut Ctx, &[Value]) -> Result<Value, ResolverError>`.
//! Register your own with [`Registry::register`]; the built-in sets live in
//! [`oc`] (OmegaConf's `oc.*`), [`builtin`] (kapitan's) and [`contrib`]
//! (general purpose helpers contributed by users). Functions from a user's
//! `resolvers.py` are bridged by [`python`].

pub mod builtin;
pub mod contrib;
pub mod oc;
pub mod python;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::{Diagnostic, Error};
use crate::interp::eval::Evaluator;
use crate::interp::parse::parse_element;
use crate::path::{Key, KeyPath};
use crate::source::{Origin, Sources};
use crate::value::{Node, Value};

#[derive(Debug)]
pub enum ResolverError {
    /// A plain failure message; the engine adds resolver name and location.
    Message(String),
    /// A fully formed diagnostic (e.g. from a nested lookup).
    Inner(Error),
}

impl From<String> for ResolverError {
    fn from(s: String) -> Self {
        ResolverError::Message(s)
    }
}

impl From<&str> for ResolverError {
    fn from(s: &str) -> Self {
        ResolverError::Message(s.to_string())
    }
}

impl From<Error> for ResolverError {
    fn from(e: Error) -> Self {
        ResolverError::Inner(e)
    }
}

pub type ResolverResult = Result<Value, ResolverError>;
pub type ResolverFn = dyn Fn(&mut Ctx, &[Value]) -> ResolverResult + Send + Sync;

#[derive(Clone, Default)]
pub struct Registry {
    map: BTreeMap<String, Arc<ResolverFn>>,
    /// Files the registry was built from (a `resolvers.py` and what it
    /// imports); when one changes the registry must be rebuilt, so the
    /// daemon restarts.
    sources: Vec<PathBuf>,
    /// Where the non-native resolvers came from, for diagnostics
    /// (`12 Python resolvers from resolvers.py via python3`).
    description: String,
}

impl Registry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything the reference kapitan knows: `oc.*`, kapitan's own
    /// resolvers and the contributed helpers.
    pub fn with_builtins() -> Self {
        let mut r = Registry::new();
        oc::register(&mut r);
        builtin::register(&mut r);
        contrib::register(&mut r);
        r
    }

    pub fn register<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut Ctx, &[Value]) -> ResolverResult + Send + Sync + 'static,
    {
        self.map.insert(name.to_string(), Arc::new(f));
    }

    pub fn register_arc(&mut self, name: &str, f: Arc<ResolverFn>) {
        self.map.insert(name.to_string(), f);
    }

    pub fn add_source(&mut self, path: PathBuf) {
        if !self.sources.contains(&path) {
            self.sources.push(path);
        }
    }

    /// Files whose change invalidates this registry.
    pub fn sources(&self) -> &[PathBuf] {
        &self.sources
    }

    pub fn is_source(&self, path: &Path) -> bool {
        self.sources.iter().any(|s| s == path)
    }

    pub fn get(&self, name: &str) -> Option<Arc<ResolverFn>> {
        self.map.get(name).cloned()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    pub fn set_description(&mut self, description: impl Into<String>) {
        self.description = description.into();
    }

    pub fn description(&self) -> &str {
        &self.description
    }
}

/// How a resolver argument was written in the expression. OmegaConf hands
/// node references to resolvers as config objects and literals as plain
/// Python values, and a few resolvers behave differently for each.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArgKind {
    /// `[..]`, `{..}` or a bare/quoted scalar written in the expression.
    Literal,
    /// `${path}`: a reference to a node of the tree.
    Node,
    /// The result of a nested resolver or a string concatenation.
    Computed,
}

/// What a resolver can see and do while it runs.
pub struct Ctx<'c, 'a> {
    pub(crate) ev: &'c mut Evaluator<'a>,
    /// Path of the node whose value is being resolved (`_node_`).
    pub at: KeyPath,
    pub origin: Origin,
    pub resolver: &'c str,
    /// One entry per argument.
    pub arg_kinds: Vec<ArgKind>,
}

impl Ctx<'_, '_> {
    pub fn arg_kind(&self, i: usize) -> ArgKind {
        self.arg_kinds.get(i).copied().unwrap_or(ArgKind::Computed)
    }

    /// Key of the node being resolved (`${key:}`), `None` at the root.
    pub fn key(&self) -> Option<Key> {
        self.ev.anchored(&self.at).last().cloned()
    }

    /// Key of the container holding the node (`${parentkey:}`).
    pub fn parent_key(&self) -> Option<Key> {
        self.ev
            .anchored(&self.at)
            .parent()
            .and_then(|p| p.last().cloned())
    }

    pub fn root(&self) -> &Node {
        self.ev.root()
    }

    pub fn sources(&self) -> &Sources {
        self.ev.sources
    }

    pub fn target(&self) -> &str {
        self.ev.target
    }

    /// Node at `key` (OmegaConf key syntax, absolute) without resolving anything.
    pub fn select_raw(&self, key: &str) -> Option<&Node> {
        crate::merge::get(self.ev.root(), &KeyPath::parse(key))
    }

    /// `OmegaConf.select(_root_, key)` followed by `to_container(resolve=True)`:
    /// the fully resolved value at `key`, or `None` when it does not exist.
    /// Keys starting with `.` are relative to the node's parent container.
    pub fn select(&mut self, key: &str) -> Result<Option<Value>, ResolverError> {
        let parent = self.at.parent().unwrap_or_default();
        let r = self.ev.select_key(&parent, key, &self.at, self.origin)?;
        match r {
            Some(r) => Ok(Some(self.ev.deep_value(r)?)),
            None => Ok(None),
        }
    }

    /// Like [`Ctx::select`] with an explicit list of keys (no splitting on dots).
    pub fn select_keys(&mut self, keys: &[String]) -> Result<Option<Value>, ResolverError> {
        let r = self
            .ev
            .select_path(KeyPath::root(), keys, &self.at, self.origin)?;
        match r {
            Some(r) => Ok(Some(self.ev.deep_value(r)?)),
            None => Ok(None),
        }
    }

    /// Evaluate a string as a grammar element (`oc.decode`).
    pub fn decode(&mut self, s: &str) -> Result<Value, ResolverError> {
        let element = parse_element(s)
            .map_err(|e| ResolverError::Message(format!("cannot decode {s:?}: {}", e.message)))?;
        let text = crate::interp::ast::Text(vec![match element {
            crate::interp::ast::Element::Prim(crate::interp::ast::Prim::Interp(i)) => {
                crate::interp::ast::TextPart::Interp(*i)
            }
            other => {
                // Wrap as a single-element resolver-less evaluation.
                let v = self.eval_element(&other)?;
                return Ok(v);
            }
        }]);
        let r = self.ev.eval_text(&text, &self.at, self.origin)?;
        Ok(self.ev.to_value(r))
    }

    fn eval_element(&mut self, el: &crate::interp::ast::Element) -> Result<Value, ResolverError> {
        // Reuse the evaluator through a synthetic resolver call shape.
        let list = crate::interp::ast::Element::List(vec![el.clone()]);
        let text = crate::interp::ast::Text(vec![crate::interp::ast::TextPart::Interp(
            crate::interp::ast::Interp::Resolver {
                name: vec![crate::interp::ast::NamePart::Lit("oc.create".into())],
                args: vec![list],
            },
        )]);
        let r = self.ev.eval_text(&text, &self.at, self.origin)?;
        match self.ev.to_value(r) {
            Value::List(mut l) if l.len() == 1 => Ok(l.pop().unwrap().value),
            other => Ok(other),
        }
    }

    /// Attach a non-fatal warning to the render.
    pub fn warn(&mut self, message: impl Into<String>) {
        let d = Diagnostic::warning(
            "resolver::warning",
            format!("resolver `{}`: {}", self.resolver, message.into()),
        )
        .with_target(self.ev.target)
        .with_path(self.at.to_string())
        .with_label(self.origin, "in this value");
        self.ev.warnings.push(d);
    }

    /// Full OmegaConf-style key of the node (`a.b[0].c`).
    pub fn full_key(&self) -> String {
        self.ev.anchored(&self.at).to_omegaconf()
    }
}

// ---- argument helpers shared by the resolver sets -------------------------

pub fn arity(name: &str, args: &[Value], min: usize, max: usize) -> Result<(), ResolverError> {
    if args.len() < min || args.len() > max {
        let expected = if min == max {
            format!("{min}")
        } else if max == usize::MAX {
            format!("at least {min}")
        } else {
            format!("{min} to {max}")
        };
        return Err(format!(
            "{name}() takes {expected} argument(s) but {} were given",
            args.len()
        )
        .into());
    }
    Ok(())
}

pub fn as_str<'v>(name: &str, args: &'v [Value], i: usize) -> Result<&'v str, ResolverError> {
    match args.get(i) {
        Some(Value::Str(s)) => Ok(s),
        Some(other) => Err(format!(
            "{name}(): argument {} must be a string, got {}",
            i + 1,
            other.type_name()
        )
        .into()),
        None => Err(format!("{name}(): missing argument {}", i + 1).into()),
    }
}

/// Python `str(x)` of an argument (resolvers written in Python often call
/// `str()` or format the argument).
pub fn as_py_str(args: &[Value], i: usize) -> String {
    args.get(i).map(Value::py_str).unwrap_or_default()
}

pub fn as_int(name: &str, args: &[Value], i: usize) -> Result<i64, ResolverError> {
    match args.get(i) {
        Some(Value::Int(n)) => Ok(*n),
        Some(Value::Bool(b)) => Ok(*b as i64),
        Some(Value::Float(f)) if f.fract() == 0.0 => Ok(*f as i64),
        Some(Value::Str(s)) => s.trim().parse().map_err(|_| {
            ResolverError::Message(format!(
                "{name}(): argument {} must be an integer, got {s:?}",
                i + 1
            ))
        }),
        Some(other) => Err(format!(
            "{name}(): argument {} must be an integer, got {}",
            i + 1,
            other.type_name()
        )
        .into()),
        None => Err(format!("{name}(): missing argument {}", i + 1).into()),
    }
}

//! Kapitan's own resolvers (`kapitan/inventory/backends/omegaconf/resolvers.py`).

use super::{Ctx, Registry, ResolverError, ResolverResult, arity, as_py_str, as_str};
use crate::emit::yaml::{DumpOptions, dump_yaml};
use crate::merge::{ListMode, MergeEvent, NoDeref, merge_with_mode};
use crate::path::Key;
use crate::value::{Map, Node, Value};

pub const LITERAL_PREFIX: &str = "__KAPITAN_LITERAL__";
pub const LITERAL_SUFFIX: &str = "__KAPITAN_LITERAL_END__";

pub fn register(r: &mut Registry) {
    r.register("key", key);
    r.register("parentkey", parentkey);
    r.register("fullkey", fullkey);
    r.register("relpath", relpath);
    r.register("access", access);
    r.register("escape", escape);
    r.register("merge", merge);
    r.register("dict", to_dict);
    r.register("list", to_list);
    r.register("yaml", to_yaml);
    r.register("add", add);
    r.register("default", default);
    r.register("write", write);
    r.register("from_file", from_file);
    for name in ["filename", "parent_filename", "path", "parent_path"] {
        r.register(name, |_, _| Ok(Value::Null));
    }
    r.register("if", cond_if);
    r.register("ifelse", cond_ifelse);
    r.register("and", cond_and);
    r.register("or", cond_or);
    r.register("not", cond_not);
    r.register("equal", cond_equal);
}

fn key_value(k: Option<Key>) -> Value {
    match k {
        Some(Key::Str(s)) => Value::Str(s),
        Some(Key::Index(i)) => Value::Int(i as i64),
        None => Value::Null,
    }
}

fn key(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("key", args, 0, 0)?;
    Ok(key_value(ctx.key()))
}

fn parentkey(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("parentkey", args, 0, 0)?;
    Ok(key_value(ctx.parent_key()))
}

fn fullkey(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("fullkey", args, 0, 0)?;
    Ok(Value::Str(ctx.full_key()))
}

/// Turn an absolute dotted path into a relative interpolation as seen from
/// the current node.
fn relpath(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("relpath", args, 1, 1)?;
    let absolute = as_str("relpath", args, 0)?;
    let path_parts: Vec<&str> = absolute.split('.').collect();
    let node_parts: Vec<String> = ctx.at.0.iter().map(Key::py_str).collect();
    let depth = node_parts.len();
    let mut relative = String::new();
    for (idx, (p, n)) in path_parts.iter().zip(node_parts.iter()).enumerate() {
        if *p != n.as_str() {
            let prefix = if idx != 0 {
                ".".repeat(depth - idx)
            } else {
                String::new()
            };
            relative = format!("{prefix}{}", path_parts[idx..].join("."));
            break;
        }
    }
    if relative.is_empty() {
        ctx.warn("self reference detected");
        return Ok(Value::Str("SELF REFERENCE DETECTED".into()));
    }
    Ok(Value::Str(format!("${{{relative}}}")))
}

/// `${access:key,with.dots,inside}`: look up a chain of literal keys.
fn access(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("access", args, 1, usize::MAX)?;
    let keys: Vec<String> = args.iter().map(Value::py_str).collect();
    match ctx.select_keys(&keys)? {
        Some(v) => Ok(v),
        None => Err(format!("key {:?} not found", keys.join("."))).map_err(ResolverError::Message),
    }
}

/// `${escape:content}` emits a literal `${content}` in the final output.
fn escape(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("escape", args, 1, 1)?;
    Ok(Value::Str(format!(
        "{LITERAL_PREFIX}{}{LITERAL_SUFFIX}",
        as_py_str(args, 0)
    )))
}

/// Replace `__KAPITAN_LITERAL__x__KAPITAN_LITERAL_END__` markers with `${x}`.
pub fn process_literals(node: &mut Node) {
    match &mut node.value {
        Value::Str(s) if s.contains(LITERAL_PREFIX) => {
            let mut out = String::with_capacity(s.len());
            let mut rest = s.as_str();
            while let Some(start) = rest.find(LITERAL_PREFIX) {
                let after = &rest[start + LITERAL_PREFIX.len()..];
                match after.find(LITERAL_SUFFIX) {
                    Some(end) if end > 0 => {
                        out.push_str(&rest[..start]);
                        out.push_str("${");
                        out.push_str(&after[..end]);
                        out.push('}');
                        rest = &after[end + LITERAL_SUFFIX.len()..];
                    }
                    _ => {
                        out.push_str(&rest[..start + LITERAL_PREFIX.len()]);
                        rest = after;
                    }
                }
            }
            out.push_str(rest);
            *s = out;
        }
        Value::List(l) => l.iter_mut().for_each(process_literals),
        Value::Map(m) => m.values_mut().for_each(process_literals),
        _ => {}
    }
}

fn merge(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("merge", args, 1, usize::MAX)?;
    let mut acc = Node::new(args[0].clone(), ctx.origin);
    if !acc.value.is_container() {
        return Err("merge(): arguments must be containers".into());
    }
    let mut log: Option<&mut Vec<MergeEvent>> = None;
    for a in &args[1..] {
        if !a.is_container() {
            return Err("merge(): arguments must be containers".into());
        }
        merge_with_mode(
            &mut acc,
            Node::new(a.clone(), ctx.origin),
            ListMode::Extend,
            &NoDeref,
            &mut log,
        );
    }
    Ok(acc.value)
}

/// `${dict:[..]}`: a literal list of dicts into one dict. The reference only
/// converts Python lists, so a list referenced from the tree (`${dict:${x}}`)
/// is returned unchanged. Each item must be a `DictConfig` or a Python
/// `list`: a referenced mapping or a literal list, not a literal `{..}`.
fn to_dict(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    use super::ArgKind;
    arity("dict", args, 1, 1)?;
    let Value::List(items) = &args[0] else {
        return Ok(args[0].clone());
    };
    let convertible = items.iter().zip(&ctx.item_kinds[0]).all(|(i, k)| match k {
        ArgKind::Literal => matches!(i.value, Value::List(_)),
        ArgKind::Node => matches!(i.value, Value::Map(_)),
        ArgKind::Computed => i.value.is_container(),
    });
    if ctx.arg_kind(0) != ArgKind::Literal || !convertible {
        return Ok(args[0].clone());
    }
    let mut out = Map::new();
    for item in items {
        if let Value::Map(m) = &item.value {
            for (k, v) in m {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    Ok(Value::Map(out))
}

/// `${list:obj}`: a dict into a list of single-entry dicts, anything else
/// through Python's `list()`.
fn to_list(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("list", args, 1, 1)?;
    match &args[0] {
        Value::Map(m) => Ok(Value::List(
            m.iter()
                .map(|(k, v)| {
                    Node::new(
                        Value::Map(Map::from_iter([(k.clone(), v.clone())])),
                        ctx.origin,
                    )
                })
                .collect(),
        )),
        Value::List(_) => Ok(args[0].clone()),
        Value::Str(s) => Ok(Value::List(
            s.chars()
                .map(|c| Node::new(Value::Str(c.to_string()), ctx.origin))
                .collect(),
        )),
        other => Err(format!("'{}' object is not iterable", other.type_name()).into()),
    }
}

/// `${yaml:key}`: `yaml.dump()` of the resolved value at `key`.
fn to_yaml(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("yaml", args, 1, 1)?;
    let key = as_str("yaml", args, 0)?.to_string();
    let v = ctx.select(&key)?.unwrap_or(Value::Null);
    Ok(Value::Str(dump_yaml(
        &Node::new(v, ctx.origin),
        &DumpOptions::pyyaml_default(),
    )))
}

/// Python `x + y`.
fn add(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("add", args, 2, 2)?;
    match (&args[0], &args[1]) {
        (Value::Int(a), Value::Int(b)) => Ok(a
            .checked_add(*b)
            .map(Value::Int)
            .unwrap_or(Value::Float(*a as f64 + *b as f64))),
        (Value::Int(a), Value::Float(b)) => Ok(Value::Float(*a as f64 + b)),
        (Value::Float(a), Value::Int(b)) => Ok(Value::Float(a + *b as f64)),
        (Value::Float(a), Value::Float(b)) => Ok(Value::Float(a + b)),
        (Value::Bool(a), Value::Int(b)) => Ok(Value::Int(*a as i64 + b)),
        (Value::Int(a), Value::Bool(b)) => Ok(Value::Int(a + *b as i64)),
        (Value::Str(a), Value::Str(b)) => Ok(Value::Str(format!("{a}{b}"))),
        (Value::List(a), Value::List(b)) => Ok(Value::List(a.iter().chain(b).cloned().collect())),
        (a, b) => Err(format!(
            "unsupported operand type(s) for +: '{}' and '{}'",
            a.type_name(),
            b.type_name()
        )
        .into()),
    }
}

/// `${default:a,b,c}` builds `${oc.select:a,${oc.select:b,c}}`, resolved on
/// the next pass.
fn default(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("default", args, 1, usize::MAX)?;
    let mut out = String::new();
    for a in &args[..args.len() - 1] {
        out.push_str("${oc.select:");
        out.push_str(&a.py_str());
        out.push(',');
    }
    out.push_str(&args[args.len() - 1].py_str());
    out.push_str(&"}".repeat(args.len() - 1));
    Ok(Value::Str(out))
}

/// `${write:destination,origin}`: kapitan's `write_to_key`. Merges the
/// resolved value at `origin` into `destination`, creating it if needed.
fn write(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("write", args, 2, 2)?;
    let destination = as_str("write", args, 0)?;
    let origin = as_str("write", args, 1)?;
    let content = match ctx.select(origin) {
        Ok(Some(v)) if !v.truthy() => return Ok(Value::Str("NOT FOUND".into())),
        Ok(Some(v @ (Value::Map(_) | Value::List(_)))) => v,
        // The reference resolves a copy of the origin, which only a container allows.
        Ok(Some(_)) => {
            ctx.warn(format!("`{origin}` is not a mapping or a list"));
            return Ok(Value::Str("ERROR WHILE RESOLVING".into()));
        }
        Ok(None) => return Ok(Value::Str("NOT FOUND".into())),
        Err(e) => {
            let message = match e {
                ResolverError::Message(m) => m,
                ResolverError::Inner(e) => e.to_string(),
            };
            ctx.warn(message);
            return Ok(Value::Str("ERROR WHILE RESOLVING".into()));
        }
    };
    ctx.write(destination, content)?;
    Ok(Value::Str("DONE".into()))
}

fn from_file(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("from_file", args, 1, 1)?;
    let path = as_str("from_file", args, 0)?;
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Value::Str(s)),
        Err(_) => {
            ctx.warn(format!("file {path} does not exist"));
            Ok(Value::Str("FILE NOT EXISTS".into()))
        }
    }
}

fn cond_if(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("if", args, 2, 2)?;
    Ok(if args[0].truthy() {
        args[1].clone()
    } else {
        Value::Map(Map::new())
    })
}

fn cond_ifelse(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("ifelse", args, 3, 3)?;
    Ok(if args[0].truthy() {
        args[1].clone()
    } else {
        args[2].clone()
    })
}

fn cond_not(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("not", args, 1, 1)?;
    Ok(Value::Bool(!args[0].truthy()))
}

fn cond_and(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    Ok(Value::Bool(args.iter().all(Value::truthy)))
}

fn cond_or(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    Ok(Value::Bool(args.iter().any(Value::truthy)))
}

fn cond_equal(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    Ok(Value::Bool(
        args.first()
            .is_none_or(|first| args.iter().all(|a| a.py_eq(first))),
    ))
}

#[cfg(test)]
mod tests {
    use crate::interp::eval::Evaluator;
    use crate::resolvers::Registry;
    use crate::source::{SourceId, Sources};
    use crate::yaml::parse_document;
    use serde_json::json;

    fn render(doc: &str) -> serde_json::Value {
        let mut root = parse_document(doc, SourceId::SYNTHETIC).unwrap();
        let registry = Registry::with_builtins();
        Evaluator::new(&mut root, &registry, &Sources::new(), "t", false)
            .resolve_all(1)
            .unwrap();
        root.value.to_json()
    }

    /// kapitan's `to_dict` converts only when every item is a `DictConfig`
    /// (a referenced mapping) or a Python `list`; a literal `{..}` is a
    /// plain dict and a referenced list a `ListConfig`.
    #[test]
    fn dict_converts_only_referenced_mappings() {
        let out = render(
            "l: [{x: 1}, {y: 2}]\nm: [1, 2]\n\
             d1: '${dict:[{a: 1},{b: 2}]}'\n\
             d2: '${dict:[${l.0},{z: 3}]}'\n\
             d3: '${dict:[${l.0},${l.1}]}'\n\
             d4: '${dict:[${l.0},${m}]}'\n",
        );
        assert_eq!(out["d1"], json!([{"a": 1}, {"b": 2}]));
        assert_eq!(out["d2"], json!([{"x": 1}, {"z": 3}]));
        assert_eq!(out["d3"], json!({"x": 1, "y": 2}));
        assert_eq!(out["d4"], json!([{"x": 1}, [1, 2]]));
    }
}

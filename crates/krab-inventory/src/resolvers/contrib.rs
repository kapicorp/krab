//! General purpose resolvers contributed by kapitan users (originally the
//! `resolvers.py` of a production inventory). Anything here is safe to use
//! from any inventory. Domain specific helpers (a cloud's region naming, the
//! shape of one repository's data) stay in that repository's `resolvers.py`:
//! a native port cannot follow the file's later edits, and with
//! `prefer-native` it would silently shadow them.

use md5::Md5;
use sha2::{Digest, Sha256};

use super::{Ctx, Registry, ResolverError, ResolverResult, arity, as_int, as_py_str, as_str};
use crate::emit::yaml::{DumpOptions, dump_yaml};
use crate::pyfmt::json_dumps;
use crate::value::{Map, Node, Value};

pub fn register(r: &mut Registry) {
    r.register("replace", replace);
    r.register("json", to_json);
    r.register("to_yaml", to_yaml);
    r.register("sha256", sha256);
    r.register("truncate", truncate);
    r.register("to_csv", to_csv);
    r.register("pluck", pluck);
    r.register("nested_dict_to_list_of_dicts", nested_dict_to_list_of_dicts);
    r.register("select_fields", select_fields);
    r.register("filter_keys", filter_keys);
    r.register("join", join);
    r.register("join_quoted", join_quoted);
}

fn replace(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("replace", args, 3, 3)?;
    let value = as_str("replace", args, 0)?;
    Ok(Value::Str(
        value.replace(&as_py_str(args, 1), &as_py_str(args, 2)),
    ))
}

fn to_json(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("json", args, 1, 1)?;
    let key = as_str("json", args, 0)?.to_string();
    let v = ctx.select(&key)?.unwrap_or(Value::Null);
    Ok(Value::Str(json_dumps(&v)))
}

/// `yaml.dump(value, default_flow_style=False, sort_keys=False).rstrip()`
fn to_yaml(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("to_yaml", args, 1, 1)?;
    let key = as_str("to_yaml", args, 0)?.to_string();
    let v = ctx.select(&key)?.unwrap_or(Value::Null);
    let opts = DumpOptions {
        sort_keys: false,
        ..DumpOptions::pyyaml_default()
    };
    Ok(Value::Str(
        dump_yaml(&Node::new(v, ctx.origin), &opts)
            .trim_end()
            .to_string(),
    ))
}

/// `${sha256:value[,length=16]}`: hex digest truncated to `length`.
fn sha256(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("sha256", args, 1, 2)?;
    let value = as_str("sha256", args, 0)?;
    let length = if args.len() > 1 {
        as_int("sha256", args, 1)?
    } else {
        16
    };
    let digest = format!("{:x}", Sha256::digest(value.as_bytes()));
    if length <= 0 || length as usize > digest.len() {
        return Ok(Value::Str(digest));
    }
    Ok(Value::Str(digest[..length as usize].to_string()))
}

/// `${truncate:value,length}`: shorten to `length` with a 4 char md5 suffix.
fn truncate(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("truncate", args, 2, 2)?;
    let value = as_str("truncate", args, 0)?;
    let length = as_int("truncate", args, 1)?;
    let chars: Vec<char> = value.chars().collect();
    if chars.len() as i64 <= length {
        return Ok(Value::Str(value.to_string()));
    }
    let hash = format!("{:x}", Md5::digest(value.as_bytes()));
    let keep = (length - 5).max(0) as usize;
    Ok(Value::Str(format!(
        "{}-{}",
        chars[..keep.min(chars.len())].iter().collect::<String>(),
        &hash[..4]
    )))
}

fn select_list(ctx: &mut Ctx, name: &str, key: &str) -> Result<Vec<Node>, ResolverError> {
    match ctx.select(key)? {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::List(l)) => Ok(l),
        Some(other) => {
            Err(format!("{name} resolver expects a list, got {}", other.type_name()).into())
        }
    }
}

fn select_dict(ctx: &mut Ctx, name: &str, key: &str) -> Result<Map, ResolverError> {
    match ctx.select(key)? {
        Some(Value::Map(m)) => Ok(m),
        Some(other) => {
            Err(format!("{name} resolver expects a dict, got {}", other.type_name()).into())
        }
        None => Err(format!("{name} resolver expects a dict, got NoneType").into()),
    }
}

fn to_csv(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("to_csv", args, 1, 1)?;
    let key = as_str("to_csv", args, 0)?.to_string();
    let items = select_list(ctx, "to_csv", &key)?;
    if items.is_empty() {
        return Ok(Value::Str(String::new()));
    }
    let Value::Map(first) = &items[0].value else {
        return Err(format!(
            "to_csv resolver expects list of dicts, got list of {}",
            items[0].value.type_name()
        )
        .into());
    };
    let mut headers: Vec<&String> = first.keys().collect();
    headers.sort();
    let mut lines = vec![
        headers
            .iter()
            .map(|h| h.as_str())
            .collect::<Vec<_>>()
            .join(","),
    ];
    for (idx, item) in items.iter().enumerate() {
        let Value::Map(m) = &item.value else {
            return Err(format!(
                "to_csv resolver: item at index {idx} is {}, expected dict",
                item.value.type_name()
            )
            .into());
        };
        let mut keys: Vec<&String> = m.keys().collect();
        keys.sort();
        if keys != headers {
            return Err(
                format!("to_csv resolver: item at index {idx} has inconsistent keys").into(),
            );
        }
        lines.push(
            headers
                .iter()
                .map(|h| m[h.as_str()].value.py_str())
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    Ok(Value::Str(lines.join("\n")))
}

fn pluck(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("pluck", args, 2, 2)?;
    let key = as_str("pluck", args, 0)?.to_string();
    let field = as_str("pluck", args, 1)?.to_string();
    let items = select_list(ctx, "pluck", &key)?;
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let Value::Map(mut m) = item.value else {
            return Err(format!(
                "pluck resolver: item at index {idx} is {}, expected dict",
                item.value.type_name()
            )
            .into());
        };
        match m.shift_remove(&field) {
            Some(v) => out.push(v),
            None => {
                return Err(format!(
                    "pluck resolver: item at index {idx} does not have field '{field}'"
                )
                .into());
            }
        }
    }
    Ok(Value::List(out))
}

fn nested_dict_to_list_of_dicts(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("nested_dict_to_list_of_dicts", args, 1, 1)?;
    let key = as_str("nested_dict_to_list_of_dicts", args, 0)?.to_string();
    let m = select_dict(ctx, "nested_dict_to_list_of_dicts", &key)?;
    Ok(Value::List(m.into_values().collect()))
}

fn select_fields(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("select_fields", args, 2, usize::MAX)?;
    let key = as_str("select_fields", args, 0)?.to_string();
    let fields: Vec<String> = args[1..].iter().map(Value::py_str).collect();
    let items = select_list(ctx, "select_fields", &key)?;
    let mut out = Vec::with_capacity(items.len());
    for (idx, item) in items.into_iter().enumerate() {
        let Value::Map(m) = &item.value else {
            return Err(format!(
                "select_fields resolver: item at index {idx} is {}, expected dict",
                item.value.type_name()
            )
            .into());
        };
        let mut picked = Map::new();
        for f in &fields {
            match m.get(f) {
                Some(v) => picked.insert(f.clone(), v.clone()),
                None => {
                    return Err(format!(
                        "select_fields resolver: item at index {idx} does not have field '{f}'"
                    )
                    .into());
                }
            };
        }
        out.push(Node::new(Value::Map(picked), item.origin));
    }
    Ok(Value::List(out))
}

/// `${filter_keys:dict,field}`: keys of the dict-of-dicts whose `field` is truthy.
fn filter_keys(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("filter_keys", args, 2, 2)?;
    let key = as_str("filter_keys", args, 0)?.to_string();
    let field = as_str("filter_keys", args, 1)?.to_string();
    let m = select_dict(ctx, "filter_keys", &key)?;
    Ok(Value::List(
        m.iter()
            .filter(|(_, v)| matches!(&v.value, Value::Map(inner) if inner.get(&field).is_some_and(|x| x.value.truthy())))
            .map(|(k, v)| Node::new(Value::Str(k.clone()), v.origin))
            .collect(),
    ))
}

fn join_with(ctx: &mut Ctx, name: &str, args: &[Value], quote: bool) -> ResolverResult {
    arity(name, args, 1, 1)?;
    let key = as_str(name, args, 0)?.to_string();
    let items = match ctx.select(&key)? {
        Some(Value::List(l)) => l,
        Some(other) => {
            return Err(
                format!("{name} resolver expects a list, got {}", other.type_name()).into(),
            );
        }
        None => return Err(format!("{name} resolver expects a list, got NoneType").into()),
    };
    let parts: Vec<String> = items
        .iter()
        .map(|n| {
            if quote {
                format!("'{}'", n.value.py_str())
            } else {
                n.value.py_str()
            }
        })
        .collect();
    Ok(Value::Str(parts.join(", ")))
}

fn join(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    join_with(ctx, "join", args, false)
}

fn join_quoted(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    join_with(ctx, "join_quoted", args, true)
}

//! OmegaConf's built-in `oc.*` resolvers.

use super::{Ctx, Registry, ResolverError, ResolverResult, arity, as_str};
use crate::value::{Node, Value};

pub fn register(r: &mut Registry) {
    r.register("oc.select", select);
    r.register("oc.env", env);
    r.register("oc.decode", decode);
    r.register("oc.create", create);
    r.register("oc.deprecated", deprecated);
    r.register("oc.dict.keys", dict_keys);
    r.register("oc.dict.values", dict_values);
}

/// `${oc.select:key[,default]}`: the value at `key`, or `default` (or `null`)
/// when it does not exist.
fn select(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("oc.select", args, 1, 2)?;
    let key = as_str("oc.select", args, 0)?.to_string();
    match ctx.select(&key)? {
        Some(v) => Ok(v),
        None => Ok(args.get(1).cloned().unwrap_or(Value::Null)),
    }
}

fn env(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("oc.env", args, 1, 2)?;
    let key = as_str("oc.env", args, 0)?;
    match std::env::var(key) {
        Ok(v) => Ok(Value::Str(v)),
        Err(_) => match args.get(1) {
            Some(Value::Null) => Ok(Value::Null),
            Some(default) => Ok(Value::Str(default.py_str())),
            None => Err(format!("environment variable '{key}' not found").into()),
        },
    }
}

fn decode(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("oc.decode", args, 1, 1)?;
    match &args[0] {
        Value::Null => Ok(Value::Null),
        Value::Str(s) => ctx.decode(s),
        other => Err(format!(
            "`oc.decode` can only take strings or None as input, but `{}` is of type {}",
            other.py_repr(),
            other.type_name()
        )
        .into()),
    }
}

fn create(_ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("oc.create", args, 1, 1)?;
    Ok(args[0].clone())
}

fn deprecated(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    arity("oc.deprecated", args, 1, 2)?;
    let key = as_str("oc.deprecated", args, 0)?.to_string();
    let message = args.get(1).map(Value::py_str).unwrap_or_else(|| {
        "'$OLD_KEY' is deprecated. Change your code and config to use '$NEW_KEY'".to_string()
    });
    let old = ctx.full_key();
    ctx.warn(message.replace("$OLD_KEY", &old).replace("$NEW_KEY", &key));
    match ctx.select(&key)? {
        Some(v) => Ok(v),
        None => Err(format!("in oc.deprecated resolver at '{old}': key not found: '{key}'").into()),
    }
}

fn dict_input(ctx: &mut Ctx, name: &str, args: &[Value]) -> Result<(String, Value), ResolverError> {
    arity(name, args, 1, 1)?;
    let key = as_str(name, args, 0)?.to_string();
    match ctx.select(&key)? {
        Some(v @ Value::Map(_)) => Ok((key, v)),
        Some(other) => Err(format!(
            "`{name}` cannot be applied to objects of type: {}",
            other.type_name()
        )
        .into()),
        None => Err(format!("key not found: '{key}'").into()),
    }
}

fn dict_keys(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    let (_, dict) = dict_input(ctx, "oc.dict.keys", args)?;
    let Value::Map(m) = dict else { unreachable!() };
    Ok(Value::List(
        m.keys()
            .map(|k| Node::new(Value::Str(k.clone()), ctx.origin))
            .collect(),
    ))
}

/// OmegaConf returns a list of `${key.k}` interpolations here; they resolve on
/// the next pass exactly like the reference.
fn dict_values(ctx: &mut Ctx, args: &[Value]) -> ResolverResult {
    let (key, dict) = dict_input(ctx, "oc.dict.values", args)?;
    let Value::Map(m) = dict else { unreachable!() };
    let key = if key.starts_with('.') {
        format!(".{key}")
    } else {
        key
    };
    Ok(Value::List(
        m.keys()
            .map(|k| Node::new(Value::Str(format!("${{{key}.{k}}}")), ctx.origin))
            .collect(),
    ))
}

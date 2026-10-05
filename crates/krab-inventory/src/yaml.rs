//! YAML loading with PyYAML `safe_load` semantics (YAML 1.1 core schema:
//! `yes`/`no` are booleans, `0755` is octal, `1e5` is a string, `1.5e3` is a
//! string too because PyYAML wants a signed exponent). Every node keeps the
//! position it was parsed from.
//!
//! Deliberate deviation: timestamps stay strings. PyYAML turns them into
//! `datetime` objects, which the reference inventory cannot represent anyway.

use std::collections::HashMap;
use std::path::Path;

use saphyr_parser::{Event, Marker, Parser, ScalarStyle, Span, Tag};

use crate::error::{Error, Result};
use crate::source::{Origin, SourceId, Sources};
use crate::value::{Map, Node, Value};

/// A top-level YAML document as an inventory file: the same loader as
/// `parse_document`, but from disk and interned in `sources`.
pub fn load_file(sources: &Sources, path: &Path) -> Result<Node> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| Error::new("io", format!("cannot read {}: {e}", path.display())))?;
    let id = sources.intern(path);
    parse_document(&text, id)
}

/// Parse the first document of `text`. An empty document yields `Null`.
pub fn parse_document(text: &str, file: SourceId) -> Result<Node> {
    let docs = parse_stream(text, file, true)?;
    Ok(docs
        .into_iter()
        .find(|d| !d.value.is_null())
        .unwrap_or_else(|| Node::new(Value::Null, Origin::new(file, 1, 1))))
}

/// Every document of a `---`-separated stream, in order; an empty document
/// yields `Null`, like PyYAML's `safe_load_all`.
pub fn parse_documents(text: &str, file: SourceId) -> Result<Vec<Node>> {
    parse_stream(text, file, false)
}

fn parse_stream(text: &str, file: SourceId, single: bool) -> Result<Vec<Node>> {
    let mut loader = Loader {
        file,
        anchors: HashMap::new(),
    };
    let mut parser = Parser::new_from_str(text);
    let mut docs = Vec::new();
    let mut current: Option<Node> = None;
    let mut seen_content = false;
    loop {
        let (event, span) = loader.next(&mut parser)?;
        match event {
            Event::StreamStart | Event::Nothing => {}
            Event::DocumentStart(_) => loader.anchors.clear(),
            Event::DocumentEnd => docs.push(
                current
                    .take()
                    .unwrap_or_else(|| Node::new(Value::Null, Origin::new(file, 1, 1))),
            ),
            Event::StreamEnd => break,
            other => {
                if single && seen_content {
                    return Err(Error::new(
                        "yaml::multiple_documents",
                        "expected a single document in the stream",
                    )
                    .with_label(loader.origin(&span), "second document starts here"));
                }
                seen_content = true;
                current = Some(loader.build(other, span, &mut parser)?);
            }
        }
    }
    if let Some(node) = current {
        docs.push(node);
    }
    Ok(docs)
}

struct Loader {
    file: SourceId,
    anchors: HashMap<usize, Node>,
}

impl Loader {
    fn origin(&self, span: &Span) -> Origin {
        marker_origin(self.file, &span.start)
    }

    fn next<'a>(
        &self,
        parser: &mut Parser<'a, saphyr_parser::StrInput<'a>>,
    ) -> Result<(Event<'a>, Span)> {
        match parser.next() {
            Some(Ok(ev)) => Ok(ev),
            Some(Err(e)) => Err(Error::new(
                "yaml::syntax",
                format!("YAML syntax error: {}", e.info()),
            )
            .with_label(marker_origin(self.file, e.marker()), "here")),
            None => Ok((Event::StreamEnd, Span::default())),
        }
    }

    fn build<'a>(
        &mut self,
        event: Event<'a>,
        span: Span,
        parser: &mut Parser<'a, saphyr_parser::StrInput<'a>>,
    ) -> Result<Node> {
        let origin = self.origin(&span);
        match event {
            Event::Scalar(text, style, anchor, tag) => {
                let value = scalar_value(&text, style, tag.as_deref(), origin)?;
                let node = Node::new(value, origin);
                if anchor != 0 {
                    self.anchors.insert(anchor, node.clone());
                }
                Ok(node)
            }
            Event::Alias(id) => self.anchors.get(&id).cloned().ok_or_else(|| {
                Error::new("yaml::unknown_alias", "alias refers to an undefined anchor")
                    .with_label(origin, "here")
            }),
            Event::SequenceStart(anchor, tag) => {
                collection_tag(tag.as_deref(), "seq", origin)?;
                let mut items = Vec::new();
                loop {
                    let (ev, sp) = self.next(parser)?;
                    if matches!(ev, Event::SequenceEnd) {
                        break;
                    }
                    items.push(self.build(ev, sp, parser)?);
                }
                let node = Node::new(Value::List(items), origin);
                if anchor != 0 {
                    self.anchors.insert(anchor, node.clone());
                }
                Ok(node)
            }
            Event::MappingStart(anchor, tag) => {
                collection_tag(tag.as_deref(), "map", origin)?;
                let mut pairs: Vec<(String, Node)> = Vec::new();
                let mut merges: Vec<Node> = Vec::new();
                loop {
                    let (kev, ksp) = self.next(parser)?;
                    if matches!(kev, Event::MappingEnd) {
                        break;
                    }
                    let key_origin = self.origin(&ksp);
                    let key = match kev {
                        Event::Scalar(text, style, _, tag) => {
                            let v = scalar_value(&text, style, tag.as_deref(), key_origin)?;
                            (
                                v.py_str(),
                                matches!(&v, Value::Str(s) if s == "<<")
                                    && style == ScalarStyle::Plain,
                            )
                        }
                        Event::Alias(id) => {
                            let v = self.anchors.get(&id).cloned().ok_or_else(|| {
                                Error::new(
                                    "yaml::unknown_alias",
                                    "alias refers to an undefined anchor",
                                )
                                .with_label(key_origin, "here")
                            })?;
                            (v.value.py_str(), false)
                        }
                        _ => {
                            return Err(Error::new(
                                "yaml::complex_key",
                                "mapping keys must be scalars",
                            )
                            .with_label(key_origin, "complex key here"));
                        }
                    };
                    let (vev, vsp) = self.next(parser)?;
                    let value = self.build(vev, vsp, parser)?;
                    if key.1 {
                        merges.push(value);
                    } else {
                        pairs.push((key.0, value));
                    }
                }
                let mut map = Map::new();
                for merge in merges {
                    flatten_merge(&mut map, merge)?;
                }
                for (k, v) in pairs {
                    map.insert(k, v);
                }
                let node = Node::new(Value::Map(map), origin);
                if anchor != 0 {
                    self.anchors.insert(anchor, node.clone());
                }
                Ok(node)
            }
            Event::SequenceEnd | Event::MappingEnd => {
                Err(Error::new("yaml::syntax", "unexpected end of collection")
                    .with_label(origin, "here"))
            }
            Event::DocumentStart(_)
            | Event::DocumentEnd
            | Event::StreamStart
            | Event::StreamEnd
            | Event::Nothing => {
                Err(Error::new("yaml::syntax", "unexpected event").with_label(origin, "here"))
            }
        }
    }
}

/// PyYAML `<<` merge: merged keys come first, the mapping's own keys win.
fn flatten_merge(into: &mut Map, merge: Node) -> Result<()> {
    match merge.value {
        Value::Map(m) => {
            for (k, v) in m {
                into.entry(k).or_insert(v);
            }
            Ok(())
        }
        Value::List(items) => {
            // Earlier entries win, so apply them last.
            for item in items.into_iter().rev() {
                match item.value {
                    Value::Map(m) => {
                        for (k, v) in m {
                            into.insert(k, v);
                        }
                    }
                    _ => {
                        return Err(Error::new("yaml::merge", "expected a mapping for merging")
                            .with_label(item.origin, "not a mapping"));
                    }
                }
            }
            Ok(())
        }
        _ => Err(Error::new(
            "yaml::merge",
            "expected a mapping or list of mappings for merging",
        )
        .with_label(merge.origin, "here")),
    }
}

fn marker_origin(file: SourceId, m: &Marker) -> Origin {
    Origin::new(file, m.line() as u32, m.col() as u32 + 1)
}

/// The tag as written: `!foo`, `!!set` (the parser expands `!!`).
fn unknown_tag(tag: &Tag, origin: Origin) -> Error {
    let handle = match tag.handle.as_str() {
        "tag:yaml.org,2002:" => "!!",
        h => h,
    };
    Error::new(
        "yaml::unknown_tag",
        format!("unsupported tag {handle}{}", tag.suffix),
    )
    .with_label(origin, "here")
}

/// A collection accepts no tag but its own core one (`!!map`, `!!seq`), which
/// PyYAML's safe loader reads as untagged; anything else fails, as in kapitan
/// (`!custom`, `!!set`) or by choice (`!!omap`, D22).
fn collection_tag(tag: Option<&Tag>, core: &str, origin: Origin) -> Result<()> {
    match tag {
        Some(t)
            if !(t.suffix == core && matches!(t.handle.as_str(), "!!" | "tag:yaml.org,2002:")) =>
        {
            Err(unknown_tag(t, origin))
        }
        _ => Ok(()),
    }
}

fn scalar_value(
    text: &str,
    style: ScalarStyle,
    tag: Option<&Tag>,
    origin: Origin,
) -> Result<Value> {
    if let Some(tag) = tag {
        let suffix = tag.suffix.as_str();
        let is_core = tag.handle == "!!" || tag.handle == "tag:yaml.org,2002:";
        if !is_core {
            return Err(unknown_tag(tag, origin));
        }
        return match suffix {
            "str" => Ok(Value::Str(text.to_string())),
            "int" => parse_int(text).ok_or_else(|| {
                Error::new("yaml::bad_int", format!("invalid integer {text:?}"))
                    .with_label(origin, "here")
            }),
            "float" => parse_float(text).ok_or_else(|| {
                Error::new("yaml::bad_float", format!("invalid float {text:?}"))
                    .with_label(origin, "here")
            }),
            "bool" => parse_bool(text).ok_or_else(|| {
                Error::new("yaml::bad_bool", format!("invalid boolean {text:?}"))
                    .with_label(origin, "here")
            }),
            "null" => Ok(Value::Null),
            _ => Err(unknown_tag(tag, origin)),
        };
    }
    if style != ScalarStyle::Plain {
        return Ok(Value::Str(text.to_string()));
    }
    Ok(resolve_plain(text))
}

/// PyYAML implicit resolution of a plain scalar.
pub fn resolve_plain(text: &str) -> Value {
    if text.is_empty() {
        return Value::Null;
    }
    match text.as_bytes()[0] {
        b'~' | b'n' | b'N' if matches!(text, "~" | "null" | "Null" | "NULL") => return Value::Null,
        _ => {}
    }
    if let Some(b) = parse_bool(text) {
        return b;
    }
    if let Some(i) = parse_int(text) {
        return i;
    }
    if let Some(f) = parse_float(text) {
        return f;
    }
    Value::Str(text.to_string())
}

fn parse_bool(text: &str) -> Option<Value> {
    match text {
        "yes" | "Yes" | "YES" | "true" | "True" | "TRUE" | "on" | "On" | "ON" => {
            Some(Value::Bool(true))
        }
        "no" | "No" | "NO" | "false" | "False" | "FALSE" | "off" | "Off" | "OFF" => {
            Some(Value::Bool(false))
        }
        _ => None,
    }
}

fn parse_int(text: &str) -> Option<Value> {
    let bytes = text.as_bytes();
    let (neg, body) = match bytes.first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    if body.is_empty() {
        return None;
    }
    let all = |s: &str, f: fn(char) -> bool| !s.is_empty() && s.chars().all(f);
    let digits = |s: &str| s.replace('_', "");
    let value: i128 = if let Some(b) = body.strip_prefix("0b") {
        if !all(b, |c| c == '0' || c == '1' || c == '_') {
            return None;
        }
        i128::from_str_radix(&digits(b), 2).ok()?
    } else if let Some(h) = body.strip_prefix("0x") {
        if !all(h, |c| c.is_ascii_hexdigit() || c == '_') {
            return None;
        }
        i128::from_str_radix(&digits(h), 16).ok()?
    } else if body.len() > 1
        && body.starts_with('0')
        && all(&body[1..], |c| ('0'..='7').contains(&c) || c == '_')
    {
        i128::from_str_radix(&digits(&body[1..]), 8).ok()?
    } else if body == "0" {
        0
    } else if body.contains(':') {
        // sexagesimal: [1-9][0-9_]*(:[0-5]?[0-9])+
        let mut parts = body.split(':');
        let head = parts.next()?;
        if !head
            .as_bytes()
            .first()
            .is_some_and(|c| (b'1'..=b'9').contains(c))
            || !all(head, |c| c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        let mut v: i128 = digits(head).parse().ok()?;
        for p in parts {
            if p.is_empty() || p.len() > 2 || !all(p, |c| c.is_ascii_digit()) {
                return None;
            }
            if p.len() == 2 && p.as_bytes()[0] > b'5' {
                return None;
            }
            v = v * 60 + p.parse::<i128>().ok()?;
        }
        v
    } else {
        if !body
            .as_bytes()
            .first()
            .is_some_and(|c| (b'1'..=b'9').contains(c))
            || !all(body, |c| c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        digits(body).parse().ok()?
    };
    let value = if neg { -value } else { value };
    Some(Value::Int(i64::try_from(value).ok()?))
}

fn parse_float(text: &str) -> Option<Value> {
    let (neg, body) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let sign = if neg { -1.0 } else { 1.0 };
    match body {
        ".inf" | ".Inf" | ".INF" => return Some(Value::Float(sign * f64::INFINITY)),
        ".nan" | ".NaN" | ".NAN" if !neg && !text.starts_with('+') => {
            return Some(Value::Float(f64::NAN));
        }
        _ => {}
    }
    // [0-9][0-9_]*\.[0-9_]*([eE][-+][0-9]+)?  |  \.[0-9][0-9_]*([eE][-+][0-9]+)?
    // | [0-9][0-9_]*(:[0-5]?[0-9])+\.[0-9_]*
    let (mantissa, exponent) = match body.find(['e', 'E']) {
        Some(i) => {
            let exp = &body[i + 1..];
            let ok = exp.len() >= 2
                && (exp.starts_with('-') || exp.starts_with('+'))
                && exp[1..].chars().all(|c| c.is_ascii_digit());
            if !ok {
                return None;
            }
            (&body[..i], Some(exp))
        }
        None => (body, None),
    };
    let dot = mantissa.find('.')?;
    let (int_part, frac_part) = (&mantissa[..dot], &mantissa[dot + 1..]);
    if !frac_part.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return None;
    }
    let value: f64 = if int_part.contains(':') {
        if exponent.is_some() {
            return None;
        }
        let mut parts = int_part.split(':');
        let head = parts.next()?;
        if head.is_empty() || !head.chars().all(|c| c.is_ascii_digit() || c == '_') {
            return None;
        }
        let mut v: f64 = head.replace('_', "").parse().ok()?;
        for p in parts {
            if p.is_empty() || p.len() > 2 || !p.chars().all(|c| c.is_ascii_digit()) {
                return None;
            }
            v = v * 60.0 + p.parse::<f64>().ok()?;
        }
        let frac = frac_part.replace('_', "");
        v + if frac.is_empty() {
            0.0
        } else {
            format!("0.{frac}").parse::<f64>().ok()?
        }
    } else {
        if int_part.is_empty() {
            if frac_part.is_empty() || !frac_part.as_bytes()[0].is_ascii_digit() {
                return None;
            }
        } else if !int_part.as_bytes()[0].is_ascii_digit()
            || !int_part.chars().all(|c| c.is_ascii_digit() || c == '_')
        {
            return None;
        }
        let mut s = String::new();
        s.push_str(&int_part.replace('_', ""));
        if s.is_empty() {
            s.push('0');
        }
        s.push('.');
        let frac = frac_part.replace('_', "");
        s.push_str(if frac.is_empty() { "0" } else { &frac });
        if let Some(e) = exponent {
            s.push('e');
            s.push_str(e);
        }
        s.parse().ok()?
    };
    Some(Value::Float(sign * value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Value {
        resolve_plain(s)
    }

    #[test]
    fn yaml11_scalars() {
        assert!(matches!(v("yes"), Value::Bool(true)));
        assert!(matches!(v("Off"), Value::Bool(false)));
        assert!(matches!(v("y"), Value::Str(_)));
        assert!(matches!(v("0755"), Value::Int(493)));
        assert!(matches!(v("0x1f"), Value::Int(31)));
        assert!(matches!(v("1_000"), Value::Int(1000)));
        assert!(matches!(v("1:30"), Value::Int(90)));
        assert!(matches!(v("1e5"), Value::Str(_)));
        assert!(matches!(v("1.5e3"), Value::Str(_)));
        assert!(matches!(v("1.5e+3"), Value::Float(f) if f == 1500.0));
        assert!(matches!(v(".5"), Value::Float(f) if f == 0.5));
        assert!(matches!(v("1."), Value::Float(f) if f == 1.0));
        assert!(matches!(v(".inf"), Value::Float(f) if f.is_infinite()));
        assert!(matches!(v("~"), Value::Null));
        assert!(matches!(v(""), Value::Null));
        assert!(matches!(v("2024-01-01"), Value::Str(_)));
        assert!(matches!(v("-"), Value::Str(_)));
        assert!(matches!(v("08"), Value::Str(_)));
    }

    #[test]
    fn non_core_tags_fail_as_written() {
        let err = |doc: &str| match parse_document(doc, SourceId(0)) {
            Ok(n) => panic!("{doc} parsed as {:?}", n.value),
            Err(e) => {
                let d = e.diagnostic().clone();
                assert_eq!(d.code, "yaml::unknown_tag", "{doc}");
                d.message
            }
        };
        assert_eq!(err("t: !custom {z: 1}\n"), "unsupported tag !custom");
        assert_eq!(err("t: !custom [1]\n"), "unsupported tag !custom");
        assert_eq!(err("s: !!set {y: null}\n"), "unsupported tag !!set");
        assert_eq!(err("m: !!omap [{x: 1}]\n"), "unsupported tag !!omap");
        assert_eq!(err("m: !!map [1]\n"), "unsupported tag !!map");
        assert_eq!(err("f: !foo bar\n"), "unsupported tag !foo");
        let ok = parse_document("m: !!map {x: 1}\ns: !!seq [1]\n", SourceId(0)).unwrap();
        assert!(ok.get("m").unwrap().as_map().is_some());
        assert!(ok.get("s").unwrap().as_list().is_some());
    }

    #[test]
    fn positions_and_merge_keys() {
        let doc = "a: 1\nb:\n  - x\n  - &anc {k: v}\nc: *anc\nd:\n  <<: *anc\n  k2: 2\n";
        let node = parse_document(doc, SourceId(0)).unwrap();
        let m = node.as_map().unwrap();
        assert_eq!(m["a"].origin.line, 1);
        assert_eq!(m["a"].origin.col, 4);
        assert_eq!(m["b"].as_list().unwrap()[1].origin.line, 4);
        assert_eq!(m["c"].get("k").unwrap().as_str(), Some("v"));
        let d = m["d"].as_map().unwrap();
        assert_eq!(d.keys().collect::<Vec<_>>(), vec!["k", "k2"]);
    }

    #[test]
    fn multi_document_streams() {
        // helm output: leading separator, `# Source:` comments, empty documents.
        let text = "---\n# Source: a.yaml\napiVersion: v1\nkind: A\n---\n---\n# Source: b.yaml\nkind: B\nvalue: '='\n";
        let docs = parse_documents(text, SourceId(0)).unwrap();
        let kinds: Vec<Option<&str>> = docs
            .iter()
            .map(|d| d.get("kind").and_then(|k| k.as_str()))
            .collect();
        assert_eq!(kinds, vec![Some("A"), None, Some("B")]);
        assert!(docs[1].value.is_null());
        assert_eq!(docs[2].get("value").unwrap().as_str(), Some("="));
        assert!(parse_documents("", SourceId(0)).unwrap().is_empty());
        // The single-document loader still rejects a second document.
        assert!(parse_document("a: 1\n---\nb: 2\n", SourceId(0)).is_err());
    }
}

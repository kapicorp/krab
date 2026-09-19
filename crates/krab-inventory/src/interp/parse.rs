//! Hand written parser for OmegaConf's interpolation grammar. It follows the
//! ANTLR grammar shipped with OmegaConf (lexer modes DEFAULT, INTERPOLATION,
//! VALUE, QUOTED_SINGLE/DOUBLE) token for token, so that typing and escaping
//! decisions match the reference implementation.

use super::ast::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub message: String,
    /// Character offset in the input.
    pub offset: usize,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} (at offset {})", self.message, self.offset)
    }
}

pub type PResult<T> = Result<T, ParseError>;

/// Parse a complete string value (grammar rule `configValue`).
pub fn parse_text(input: &str) -> PResult<Text> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    let text = p.text(None)?;
    if p.pos != p.chars.len() {
        return Err(p.err("unexpected trailing input"));
    }
    Ok(text)
}

/// Parse a single element (grammar rule `singleElement`), used by `oc.decode`.
pub fn parse_element(input: &str) -> PResult<Element> {
    let mut p = Parser {
        chars: input.chars().collect(),
        pos: 0,
    };
    p.skip_ws();
    let e = p.element()?;
    p.skip_ws();
    if p.pos != p.chars.len() {
        return Err(p.err("unexpected trailing input"));
    }
    Ok(e)
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

const ESCAPABLE: &str = "\\()[]{}:=, \t";

impl Parser {
    fn err(&self, msg: &str) -> ParseError {
        ParseError {
            message: msg.to_string(),
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.chars.get(self.pos + n).copied()
    }

    fn at(&self, s: &str) -> bool {
        s.chars()
            .enumerate()
            .all(|(i, c)| self.peek_at(i) == Some(c))
    }

    fn bump(&mut self) -> Option<char> {
        let c = self.peek();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t')) {
            self.pos += 1;
        }
    }

    fn count_backslashes(&self) -> usize {
        let mut n = 0;
        while self.peek_at(n) == Some('\\') {
            n += 1;
        }
        n
    }

    // ---- text (DEFAULT_MODE and the quoted modes) -------------------------

    /// `text: (interpolation | ANY_STR | ESC | ESC_INTER | TOP_ESC | QUOTED_ESC)+`
    /// Stops before `quote` when given (quoted value), otherwise at end of input.
    fn text(&mut self, quote: Option<char>) -> PResult<Text> {
        let mut parts: Vec<TextPart> = Vec::new();
        let mut lit = String::new();
        loop {
            let Some(c) = self.peek() else {
                if quote.is_some() {
                    return Err(self.err("unterminated quoted string"));
                }
                break;
            };
            if Some(c) == quote {
                break;
            }
            if self.at("${") {
                flush(&mut parts, &mut lit);
                self.pos += 2;
                parts.push(TextPart::Interp(self.interpolation()?));
                continue;
            }
            if c == '\\' {
                let n = self.count_backslashes();
                let after = self.peek_at(n);
                let after2 = self.peek_at(n + 1);
                if after == Some('$') && after2 == Some('{') {
                    if n % 2 == 1 {
                        // ESC_INTER: `\${` (with pairs of escaped backslashes before)
                        lit.extend(std::iter::repeat_n('\\', n / 2));
                        lit.push_str("${");
                        self.pos += n + 2;
                    } else {
                        // Even backslashes before an interpolation: unescape.
                        lit.extend(std::iter::repeat_n('\\', n / 2));
                        self.pos += n;
                    }
                    continue;
                }
                if let Some(q) = quote {
                    if after == Some(q) && n % 2 == 1 {
                        // Escaped quote (`\'` inside '...'): unescape.
                        lit.extend(std::iter::repeat_n('\\', n / 2));
                        lit.push(q);
                        self.pos += n + 1;
                        continue;
                    }
                    if n.is_multiple_of(2) && after == Some(q) {
                        // QUOTED_ESC at the end of the string: unescape.
                        lit.extend(std::iter::repeat_n('\\', n / 2));
                        self.pos += n;
                        continue;
                    }
                }
                lit.extend(std::iter::repeat_n('\\', n));
                self.pos += n;
                continue;
            }
            lit.push(c);
            self.pos += 1;
        }
        flush(&mut parts, &mut lit);
        Ok(Text(parts))
    }

    // ---- interpolation (INTERPOLATION_MODE) -------------------------------

    /// Called after `${` has been consumed.
    fn interpolation(&mut self) -> PResult<Interp> {
        self.skip_ws();
        let mut dots = 0;
        while self.peek() == Some('.') {
            dots += 1;
            self.pos += 1;
        }
        // Collect head segments until `:` (resolver) or `}` (node).
        #[derive(Debug)]
        enum Tok {
            Dot,
            Key(KeySeg),
            Bracket(KeySeg),
        }
        let mut toks: Vec<Tok> = Vec::new();
        loop {
            match self.peek() {
                None => return Err(self.err("unterminated interpolation")),
                Some(' ' | '\t') => {
                    let save = self.pos;
                    self.skip_ws();
                    match self.peek() {
                        Some('}') => {
                            self.pos += 1;
                            return self.node_interp(dots, toks_to_keys(toks, self)?);
                        }
                        Some(':') => {
                            self.pos += 1;
                            self.skip_ws();
                            return self.resolver_interp(dots, toks_to_name(toks, self, save)?);
                        }
                        _ => {
                            self.pos = save;
                            return Err(self.err("unexpected whitespace in interpolation"));
                        }
                    }
                }
                Some('}') => {
                    self.pos += 1;
                    return self.node_interp(dots, toks_to_keys(toks, self)?);
                }
                Some(':') => {
                    let save = self.pos;
                    self.pos += 1;
                    self.skip_ws();
                    return self.resolver_interp(dots, toks_to_name(toks, self, save)?);
                }
                Some('.') => {
                    self.pos += 1;
                    toks.push(Tok::Dot);
                }
                Some('[') => {
                    self.pos += 1;
                    let seg = self.config_key()?;
                    if self.bump() != Some(']') {
                        return Err(self.err("expected `]`"));
                    }
                    toks.push(Tok::Bracket(seg));
                }
                Some(_) => {
                    let seg = self.config_key()?;
                    toks.push(Tok::Key(seg));
                }
            }
        }

        fn toks_to_keys(toks: Vec<Tok>, p: &Parser) -> PResult<Vec<KeySeg>> {
            // (configKey | [configKey]) (DOT configKey | [configKey])*
            let mut keys = Vec::new();
            let mut expect_key = true;
            for t in toks {
                match t {
                    Tok::Dot => {
                        if expect_key {
                            return Err(p.err("unexpected `.` in interpolation"));
                        }
                        expect_key = true;
                    }
                    Tok::Key(k) => {
                        if !expect_key {
                            return Err(p.err("expected `.` or `[` in interpolation"));
                        }
                        keys.push(k);
                        expect_key = false;
                    }
                    Tok::Bracket(k) => {
                        if expect_key && !keys.is_empty() {
                            return Err(p.err("unexpected `[` after `.`"));
                        }
                        keys.push(k);
                        expect_key = false;
                    }
                }
            }
            if keys.is_empty() || expect_key {
                return Err(p.err("empty interpolation key"));
            }
            Ok(keys)
        }

        fn toks_to_name(toks: Vec<Tok>, _p: &Parser, at: usize) -> PResult<Vec<NamePart>> {
            let mut name = Vec::new();
            let mut expect = true;
            for t in toks {
                match (t, expect) {
                    (Tok::Dot, false) => expect = true,
                    (Tok::Key(KeySeg::Lit(s)), true) => {
                        if !is_id(&s) {
                            return Err(ParseError {
                                message: format!("invalid resolver name `{s}`"),
                                offset: at,
                            });
                        }
                        name.push(NamePart::Lit(s));
                        expect = false;
                    }
                    (Tok::Key(KeySeg::Interp(i)), true) => {
                        name.push(NamePart::Interp(i));
                        expect = false;
                    }
                    _ => {
                        return Err(ParseError {
                            message: "invalid resolver name".into(),
                            offset: at,
                        });
                    }
                }
            }
            if name.is_empty() || expect {
                return Err(ParseError {
                    message: "empty resolver name".into(),
                    offset: at,
                });
            }
            Ok(name)
        }
    }

    fn node_interp(&mut self, dots: usize, keys: Vec<KeySeg>) -> PResult<Interp> {
        Ok(Interp::Node { dots, keys })
    }

    fn resolver_interp(&mut self, dots: usize, name: Vec<NamePart>) -> PResult<Interp> {
        if dots > 0 {
            return Err(self.err("resolver names cannot start with `.`"));
        }
        // VALUE_MODE: `sequence? BRACE_CLOSE`
        let args = if self.peek() == Some('}') {
            Vec::new()
        } else {
            self.sequence('}')?
        };
        if self.bump() != Some('}') {
            return Err(self.err("expected `}` to close resolver interpolation"));
        }
        Ok(Interp::Resolver { name, args })
    }

    /// `configKey: interpolation | ID | INTER_KEY`
    fn config_key(&mut self) -> PResult<KeySeg> {
        if self.at("${") {
            self.pos += 2;
            return Ok(KeySeg::Interp(Box::new(self.interpolation()?)));
        }
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if "\\{}()[]:. \t'\"".contains(c) || (c == '$' && self.peek_at(1) == Some('{')) {
                break;
            }
            s.push(c);
            self.pos += 1;
        }
        if s.is_empty() {
            return Err(self.err("expected a key in interpolation"));
        }
        Ok(KeySeg::Lit(s))
    }

    // ---- VALUE_MODE ------------------------------------------------------

    /// `sequence: (element (COMMA element?)*) | (COMMA element?)+`, stopping
    /// before `close` (not consumed). Missing elements become empty strings.
    fn sequence(&mut self, close: char) -> PResult<Vec<Element>> {
        let mut items = Vec::new();
        let mut prev_comma = true;
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(self.err("unterminated sequence")),
                Some(c) if c == close => {
                    if prev_comma && !items.is_empty() {
                        items.push(Element::Prim(Prim::Str(String::new())));
                    }
                    return Ok(items);
                }
                Some(',') => {
                    self.pos += 1;
                    if prev_comma {
                        items.push(Element::Prim(Prim::Str(String::new())));
                    }
                    prev_comma = true;
                }
                Some(_) => {
                    if !prev_comma {
                        return Err(self.err("expected `,`"));
                    }
                    items.push(self.element()?);
                    prev_comma = false;
                }
            }
        }
    }

    /// `element: primitive | quotedValue | listContainer | dictContainer`
    fn element(&mut self) -> PResult<Element> {
        match self.peek() {
            Some(q @ ('\'' | '"')) => {
                self.pos += 1;
                let t = self.text(Some(q))?;
                self.pos += 1; // closing quote
                Ok(Element::Quoted(t))
            }
            Some('[') => {
                self.pos += 1;
                self.skip_ws();
                let items = if self.peek() == Some(']') {
                    Vec::new()
                } else {
                    self.sequence(']')?
                };
                self.skip_ws();
                if self.bump() != Some(']') {
                    return Err(self.err("expected `]`"));
                }
                Ok(Element::List(items))
            }
            Some('{') => {
                self.pos += 1;
                let mut pairs = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        None => return Err(self.err("unterminated dict")),
                        Some('}') => {
                            self.pos += 1;
                            break;
                        }
                        Some(',') if !pairs.is_empty() => {
                            self.pos += 1;
                        }
                        Some(_) => {
                            let key = self.primitive(true)?;
                            self.skip_ws();
                            if self.bump() != Some(':') {
                                return Err(self.err("expected `:` in dict"));
                            }
                            self.skip_ws();
                            let value = self.element()?;
                            pairs.push((key, value));
                        }
                    }
                }
                Ok(Element::Dict(pairs))
            }
            _ => Ok(Element::Prim(self.primitive(false)?)),
        }
    }

    /// `primitive: (ID | NULL | INT | FLOAT | BOOL | UNQUOTED_CHAR | COLON | ESC | WS | interpolation)+`
    /// Tokenised with ANTLR longest-match rules; a lone token is typed.
    fn primitive(&mut self, dict_key: bool) -> PResult<Prim> {
        #[derive(Debug)]
        enum Tok {
            Typed(Prim),
            Text(String),
            Interp(Interp),
        }
        let mut toks: Vec<Tok> = Vec::new();
        while let Some(c) = self.peek() {
            // Delimiters (with the whitespace that belongs to them).
            if c == ' ' || c == '\t' {
                let save = self.pos;
                self.skip_ws();
                match self.peek() {
                    Some(',' | '}' | ']') | None => break,
                    Some(':') if !dict_key => {
                        self.pos += 1;
                        self.skip_ws();
                        toks.push(Tok::Text(self.chars[save..self.pos].iter().collect()));
                        continue;
                    }
                    Some(':') => {
                        self.pos = save;
                        break;
                    }
                    _ => {
                        toks.push(Tok::Text(self.chars[save..self.pos].iter().collect()));
                        continue;
                    }
                }
            }
            if matches!(c, ',' | '}' | ']') {
                break;
            }
            if c == ':' {
                if dict_key {
                    break;
                }
                let start = self.pos;
                self.pos += 1;
                self.skip_ws();
                toks.push(Tok::Text(self.chars[start..self.pos].iter().collect()));
                continue;
            }
            if matches!(c, '\'' | '"' | '[' | '{') {
                return Err(self.err(&format!("unexpected `{c}` in unquoted value")));
            }
            if self.at("${") {
                if dict_key {
                    return Err(self.err("interpolations are not allowed in dict keys"));
                }
                self.pos += 2;
                toks.push(Tok::Interp(self.interpolation()?));
                continue;
            }
            if c == '\\' {
                // ESC: one or more `\X` with X escapable; else a lone `\` is UNQUOTED_CHAR.
                let mut out = String::new();
                let mut i = self.pos;
                while self.chars.get(i) == Some(&'\\') {
                    match self.chars.get(i + 1) {
                        Some(x) if ESCAPABLE.contains(*x) => {
                            out.push(*x);
                            i += 2;
                        }
                        _ => break,
                    }
                }
                if out.is_empty() {
                    self.pos += 1;
                    toks.push(Tok::Text("\\".into()));
                } else {
                    self.pos = i;
                    toks.push(Tok::Typed(Prim::Str(out)));
                    // ESC alone yields its unescaped text; mark as text-like typed str.
                }
                continue;
            }
            if let Some((len, prim)) = self.number() {
                self.pos += len;
                toks.push(Tok::Typed(prim));
                continue;
            }
            if c.is_ascii_alphabetic() || c == '_' {
                let start = self.pos;
                while matches!(self.peek(), Some(ch) if ch.is_ascii_alphanumeric() || ch == '_') {
                    self.pos += 1;
                }
                let word: String = self.chars[start..self.pos].iter().collect();
                let lower = word.to_ascii_lowercase();
                let prim = match lower.as_str() {
                    "true" => Prim::Bool(true),
                    "false" => Prim::Bool(false),
                    "null" => Prim::Null,
                    "inf" => Prim::Float(f64::INFINITY),
                    "nan" => Prim::Float(f64::NAN),
                    _ => Prim::Str(word),
                };
                toks.push(Tok::Typed(prim));
                continue;
            }
            // UNQUOTED_CHAR or (leniently) any other character.
            self.pos += 1;
            toks.push(Tok::Text(c.to_string()));
        }
        if toks.is_empty() {
            return Err(self.err("expected a value"));
        }
        if toks.len() == 1 {
            return Ok(match toks.pop().unwrap() {
                Tok::Typed(p) => p,
                Tok::Text(s) => Prim::Str(s),
                Tok::Interp(i) => Prim::Interp(Box::new(i)),
            });
        }
        let mut parts: Vec<TextPart> = Vec::new();
        let mut lit = String::new();
        for t in toks {
            match t {
                Tok::Interp(i) => {
                    flush(&mut parts, &mut lit);
                    parts.push(TextPart::Interp(i));
                }
                Tok::Text(s) => lit.push_str(&s),
                Tok::Typed(p) => lit.push_str(&prim_source(&p)),
            }
        }
        flush(&mut parts, &mut lit);
        if parts.iter().all(|p| matches!(p, TextPart::Lit(_))) {
            let s = parts
                .into_iter()
                .map(|p| match p {
                    TextPart::Lit(s) => s,
                    _ => unreachable!(),
                })
                .collect();
            return Ok(Prim::Str(s));
        }
        Ok(Prim::Concat(parts))
    }

    /// Longest match of INT / FLOAT at the current position. Returns the token
    /// length in chars and the typed value.
    fn number(&self) -> Option<(usize, Prim)> {
        let s = &self.chars[self.pos..];
        let mut i = 0;
        if matches!(s.first(), Some('+' | '-')) {
            i += 1;
        }
        let digit = |c: Option<&char>| c.is_some_and(|c| c.is_ascii_digit());
        // INT_UNSIGNED: '0' | [1-9] (('_')? DIGIT)*
        let int_unsigned = |mut j: usize| -> Option<usize> {
            match s.get(j) {
                Some('0') => Some(j + 1),
                Some(c) if ('1'..='9').contains(c) => {
                    j += 1;
                    loop {
                        if digit(s.get(j)) {
                            j += 1;
                        } else if s.get(j) == Some(&'_') && digit(s.get(j + 1)) {
                            j += 2;
                        } else {
                            break;
                        }
                    }
                    Some(j)
                }
                _ => None,
            }
        };
        // DIGIT (('_')? DIGIT)*
        let digits = |mut j: usize| -> Option<usize> {
            if !digit(s.get(j)) {
                return None;
            }
            j += 1;
            loop {
                if digit(s.get(j)) {
                    j += 1;
                } else if s.get(j) == Some(&'_') && digit(s.get(j + 1)) {
                    j += 2;
                } else {
                    break;
                }
            }
            Some(j)
        };
        let int_end = int_unsigned(i);
        // POINT_FLOAT: INT_UNSIGNED '.' | INT_UNSIGNED? '.' DIGIT (('_')? DIGIT)*
        let mut point_end: Option<usize> = None;
        if let Some(e) = int_end {
            if s.get(e) == Some(&'.') {
                point_end = Some(digits(e + 1).unwrap_or(e + 1));
            }
        } else if s.get(i) == Some(&'.') {
            point_end = digits(i + 1);
        }
        // EXPONENT_FLOAT: (INT_UNSIGNED | POINT_FLOAT) [eE] [+-]? DIGIT (('_')? DIGIT)*
        let mut exp_end: Option<usize> = None;
        for base in [point_end, int_end].into_iter().flatten() {
            if matches!(s.get(base), Some('e' | 'E')) {
                let mut j = base + 1;
                if matches!(s.get(j), Some('+' | '-')) {
                    j += 1;
                }
                if let Some(e) = digits(j) {
                    exp_end = Some(exp_end.map_or(e, |x: usize| x.max(e)));
                }
            }
        }
        let float_end = [exp_end, point_end].into_iter().flatten().max();
        let text = |end: usize| -> String { s[..end].iter().collect() };
        match (float_end, int_end) {
            (Some(fe), ie) if ie.is_none_or(|ie| fe >= ie) => {
                let t = text(fe).replace('_', "");
                Some((fe, Prim::Float(t.parse().ok()?)))
            }
            (_, Some(ie)) => {
                let t = text(ie).replace('_', "");
                match t.parse::<i64>() {
                    Ok(v) => Some((ie, Prim::Int(v))),
                    Err(_) => Some((ie, Prim::Float(t.parse().ok()?))),
                }
            }
            _ => None,
        }
    }
}

fn flush(parts: &mut Vec<TextPart>, lit: &mut String) {
    if !lit.is_empty() {
        parts.push(TextPart::Lit(std::mem::take(lit)));
    }
}

fn is_id(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Source text of a typed token when it takes part in a concatenation. Typed
/// tokens keep their original spelling in OmegaConf (`1_0` stays `1_0`), which
/// we cannot recover after typing; the canonical spelling is used instead.
fn prim_source(p: &Prim) -> String {
    match p {
        Prim::Null => "null".into(),
        Prim::Bool(true) => "true".into(),
        Prim::Bool(false) => "false".into(),
        Prim::Int(i) => i.to_string(),
        Prim::Float(f) => crate::value::py_float_repr(*f),
        Prim::Str(s) => s.clone(),
        Prim::Interp(i) => i.to_string(),
        Prim::Concat(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> TextPart {
        TextPart::Lit(s.into())
    }

    fn node(keys: &[&str]) -> Interp {
        Interp::Node {
            dots: 0,
            keys: keys.iter().map(|k| KeySeg::Lit(k.to_string())).collect(),
        }
    }

    #[test]
    fn plain_and_node() {
        assert_eq!(parse_text("hello").unwrap(), Text(vec![lit("hello")]));
        assert_eq!(
            parse_text("${a.b}").unwrap(),
            Text(vec![TextPart::Interp(node(&["a", "b"]))])
        );
        assert_eq!(
            parse_text("x-${a}-y").unwrap(),
            Text(vec![lit("x-"), TextPart::Interp(node(&["a"])), lit("-y")])
        );
        assert_eq!(
            parse_text("${_kapitan_.name.parts[1]}")
                .unwrap()
                .single_interp(),
            Some(&node(&["_kapitan_", "name", "parts", "1"]))
        );
        assert_eq!(
            parse_text("${.x}").unwrap().single_interp(),
            Some(&Interp::Node {
                dots: 1,
                keys: vec![KeySeg::Lit("x".into())]
            })
        );
        assert_eq!(
            parse_text("${ a }").unwrap().single_interp(),
            Some(&node(&["a"]))
        );
        assert_eq!(
            parse_text("${foo-bar.baz}").unwrap().single_interp(),
            Some(&node(&["foo-bar", "baz"]))
        );
    }

    #[test]
    fn nested_key() {
        let t = parse_text("${permissions.${loc}.iam}").unwrap();
        match t.single_interp().unwrap() {
            Interp::Node { keys, .. } => {
                assert_eq!(keys.len(), 3);
                assert!(matches!(&keys[1], KeySeg::Interp(_)));
            }
            _ => panic!(),
        }
    }

    fn resolver(input: &str) -> (String, Vec<Element>) {
        match parse_text(input).unwrap().single_interp().unwrap() {
            Interp::Resolver { name, args } => {
                let n = name
                    .iter()
                    .map(|p| match p {
                        NamePart::Lit(s) => s.clone(),
                        _ => "?".into(),
                    })
                    .collect::<Vec<_>>()
                    .join(".");
                (n, args.clone())
            }
            other => panic!("not a resolver: {other:?}"),
        }
    }

    #[test]
    fn resolver_args() {
        let (n, a) = resolver("${oc.select:a.b, 3}");
        assert_eq!(n, "oc.select");
        assert_eq!(
            a,
            vec![
                Element::Prim(Prim::Str("a.b".into())),
                Element::Prim(Prim::Int(3))
            ]
        );

        let (n, a) = resolver("${parentkey:}");
        assert_eq!(n, "parentkey");
        assert!(a.is_empty());

        let (_, a) = resolver("${replace:${target},.,-}");
        assert_eq!(a.len(), 3);
        assert!(matches!(&a[0], Element::Prim(Prim::Interp(_))));
        assert_eq!(a[1], Element::Prim(Prim::Str(".".into())));

        let (_, a) = resolver(r#"${replace:${target},".","-"}"#);
        assert_eq!(a[1], Element::Quoted(Text(vec![lit(".")])));

        let (_, a) = resolver("${truncate:${cluster.name}-distribution,30}");
        assert!(matches!(&a[0], Element::Prim(Prim::Concat(parts)) if parts.len() == 2));
        assert_eq!(a[1], Element::Prim(Prim::Int(30)));

        let (_, a) = resolver("${ifelse:${.folder_id},null,${gcp_organization_id}}");
        assert_eq!(a[1], Element::Prim(Prim::Null));

        let (_, a) = resolver("${f:[1, 2.5, true, x], {k: v, q: ${z}}, a b}");
        assert!(
            matches!(&a[0], Element::List(l) if l.len() == 4 && l[1] == Element::Prim(Prim::Float(2.5)))
        );
        assert!(matches!(&a[1], Element::Dict(d) if d.len() == 2));
        assert_eq!(a[2], Element::Prim(Prim::Str("a b".into())));

        let (_, a) =
            resolver("${escape:'cidrsubnet(\"172.16.0.0/22\", 6, ${cluster.params.num_id})'}");
        assert!(matches!(&a[0], Element::Quoted(Text(parts)) if parts.len() == 3));

        let (_, a) = resolver("${f:a\\,b, 1-2, 01, 1_000, -1, inf, info, .5, 1.}");
        assert_eq!(a[0], Element::Prim(Prim::Str("a,b".into())));
        assert_eq!(a[1], Element::Prim(Prim::Str("1-2".into())));
        assert_eq!(a[2], Element::Prim(Prim::Str("01".into())));
        assert_eq!(a[3], Element::Prim(Prim::Int(1000)));
        assert_eq!(a[4], Element::Prim(Prim::Int(-1)));
        assert!(matches!(a[5], Element::Prim(Prim::Float(f)) if f.is_infinite()));
        assert_eq!(a[6], Element::Prim(Prim::Str("info".into())));
        assert_eq!(a[7], Element::Prim(Prim::Float(0.5)));
        assert_eq!(a[8], Element::Prim(Prim::Float(1.0)));

        let (_, a) = resolver("${f:a : b}");
        assert_eq!(a[0], Element::Prim(Prim::Str("a : b".into())));
    }

    #[test]
    fn escapes() {
        assert_eq!(parse_text(r"\${a}").unwrap(), Text(vec![lit("${a}")]));
        assert_eq!(
            parse_text(r"\\${a}").unwrap(),
            Text(vec![lit("\\"), TextPart::Interp(node(&["a"]))])
        );
        assert_eq!(parse_text(r"a\\b").unwrap(), Text(vec![lit("a\\\\b")]));
        assert_eq!(
            parse_text("$5 and $ {x}").unwrap(),
            Text(vec![lit("$5 and $ {x}")])
        );
        let (_, a) = resolver(r"${f:'it\'s'}");
        assert_eq!(a[0], Element::Quoted(Text(vec![lit("it's")])));
    }

    #[test]
    fn errors() {
        assert!(parse_text("${a").is_err());
        assert!(parse_text("${a:b").is_err());
        assert!(parse_text("${}").is_err());
        assert!(parse_text("${f:'x}").is_err());
    }
}

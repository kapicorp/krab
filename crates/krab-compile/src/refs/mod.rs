//! Kapitan references (`?{type:path}` tags), mirroring `kapitan/refs` of
//! kapitan 0.36.3: compiling tags into output (the short `?{type:path:hash}`
//! form, the `--embed-refs` payload, or a fresh ref created from
//! `||functions`), revealing them (`--reveal`, `krab refs --reveal`) and
//! writing them (`krab refs --write`).
//!
//! Ref files are YAML mappings under the refs path (`data`, `encoding`,
//! `type`, plus `key` for the KMS backends, `recipients` for gpg and
//! `vault_params` for Vault). `data` holds base64 text for every type but
//! `plain` and `env`, which store their text as is.

pub mod functions;
pub mod gkms;
pub mod gpg;
pub mod kms_cli;
pub mod vault;

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use base64::Engine as _;
use krab_inventory::emit::yaml::{DumpOptions, dump_yaml};
use krab_inventory::pyfmt::json_dumps;
use krab_inventory::source::SourceId;
use krab_inventory::yaml::parse_document;
use krab_inventory::{Map, Node, Value};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::Value as Json;
use sha2::{Digest, Sha256};

use crate::inputs::Reads;

/// `?{ref:my/secret/token}`, `?{ref:my/secret/token||random:str}`,
/// `?{ref:payload:embedded}`; group 1 = whole tag, 2 = `type:path`, 3 = functions.
pub fn tag_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(\?\{(\w+:[\w\-\.\@\=\/\:]+)(\|(?:(?:\|\w+)(?::\S*)*)+)?\=*\})").unwrap()
    })
}

/// `@sub.var` inside a tag: a sub-variable of a YAML secret.
fn subvar_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(@[\w\.\-\_]+)").unwrap())
}

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

pub fn b64_encode(data: &[u8]) -> String {
    B64.encode(data)
}

pub fn b64_decode(text: &str) -> Result<Vec<u8>, RefError> {
    B64.decode(text.trim())
        .map_err(|e| RefError(format!("invalid base64 in reference data: {e}")))
}

#[derive(Debug, Clone)]
pub struct RefError(pub String);

impl std::fmt::Display for RefError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RefError {}

/// kapitan's `KapitanReferencesTypes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RefType {
    Gpg,
    VaultKv,
    VaultTransit,
    AwsKms,
    Gkms,
    AzKms,
    Base64,
    Plain,
    Env,
}

impl RefType {
    pub const NAMES: &'static str = "gpg/gkms/awskms/azkms/vaultkv/vaulttransit/base64/plain/env";

    pub fn parse(s: &str) -> Option<RefType> {
        Some(match s {
            "gpg" => RefType::Gpg,
            "vaultkv" => RefType::VaultKv,
            "vaulttransit" => RefType::VaultTransit,
            "awskms" => RefType::AwsKms,
            "gkms" => RefType::Gkms,
            "azkms" => RefType::AzKms,
            "base64" => RefType::Base64,
            "plain" => RefType::Plain,
            "env" => RefType::Env,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            RefType::Gpg => "gpg",
            RefType::VaultKv => "vaultkv",
            RefType::VaultTransit => "vaulttransit",
            RefType::AwsKms => "awskms",
            RefType::Gkms => "gkms",
            RefType::AzKms => "azkms",
            RefType::Base64 => "base64",
            RefType::Plain => "plain",
            RefType::Env => "env",
        }
    }

    pub fn is_kms(self) -> bool {
        matches!(self, RefType::Gkms | RefType::AwsKms | RefType::AzKms)
    }

    pub fn is_vault(self) -> bool {
        matches!(self, RefType::VaultKv | RefType::VaultTransit)
    }
}

impl std::fmt::Display for RefType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.name())
    }
}

/// A reference: what a ref file or an embedded payload holds.
#[derive(Clone, Debug)]
pub struct Ref {
    pub type_name: RefType,
    /// The `data` field as stored.
    pub data: String,
    /// `original` or `base64` (the plaintext was base64 encoded before storing).
    pub encoding: String,
    /// KMS key (`gkms`, `awskms`, `azkms`).
    pub key: Option<String>,
    /// GPG recipient fingerprints, sorted and unique (kapitan resolves names
    /// to fingerprints when it loads a ref).
    pub recipients: Vec<String>,
    /// `vault_params` of the Vault backends, every field present, in the
    /// order kapitan's model declares them.
    pub vault_params: Option<Json>,
    pub embedded_subvar_path: Option<String>,
    /// The tag path this was loaded under (`a/b` or `a/b@sub`); `None` for
    /// embedded payloads.
    pub path: Option<String>,
    /// sha256 of the file path and `data`; `None` for embedded payloads.
    pub hash: Option<String>,
}

impl Ref {
    pub fn new(type_name: RefType, data: String, encoding: &str) -> Ref {
        Ref {
            type_name,
            data,
            encoding: encoding.to_string(),
            key: None,
            recipients: Vec::new(),
            vault_params: None,
            embedded_subvar_path: None,
            path: None,
            hash: None,
        }
    }

    /// kapitan's `dump()`: the fields written to a ref file, in its order
    /// (which happens to be alphabetical for every type, so files written
    /// with `yaml.safe_dump` and embedded payloads agree).
    pub fn dump(&self) -> Map {
        let mut m = Map::new();
        let s = |v: &str| Node::synthetic(Value::Str(v.to_string()));
        m.insert("data".into(), s(&self.data));
        m.insert("encoding".into(), s(&self.encoding));
        match self.type_name {
            RefType::Gkms | RefType::AwsKms | RefType::AzKms => {
                m.insert(
                    "key".into(),
                    Node::synthetic(match &self.key {
                        Some(k) => Value::Str(k.clone()),
                        None => Value::Null,
                    }),
                );
            }
            RefType::Gpg => {
                let list = self
                    .recipients
                    .iter()
                    .map(|f| {
                        let mut r = Map::new();
                        r.insert("fingerprint".into(), s(f));
                        Node::synthetic(Value::Map(r))
                    })
                    .collect();
                m.insert("recipients".into(), Node::synthetic(Value::List(list)));
            }
            _ => {}
        }
        m.insert("type".into(), s(self.type_name.name()));
        if self.type_name.is_vault() {
            let params = self
                .vault_params
                .clone()
                .unwrap_or_else(|| vault::normalize_params(None, self.type_name));
            m.insert("vault_params".into(), Node::synthetic(Value::from(params)));
        }
        m
    }

    /// The YAML kapitan writes for this ref (`yaml.safe_dump`, sorted keys).
    pub fn to_yaml(&self) -> String {
        dump_yaml(
            &Node::synthetic(Value::Map(self.dump())),
            &DumpOptions::pyyaml_default(),
        )
    }

    fn compile_embedded(&self) -> String {
        let mut dump = self.dump();
        if let Some((_, sub)) = self.path.as_deref().and_then(|p| p.split_once('@')) {
            dump.insert(
                "embedded_subvar_path".into(),
                Node::synthetic(Value::Str(sub.to_string())),
            );
        }
        let payload = b64_encode(json_dumps(&Value::Map(dump)).as_bytes());
        format!("?{{{}:{payload}:embedded}}", self.type_name)
    }

    fn short_hash(&self) -> Result<&str, RefError> {
        self.hash
            .as_deref()
            .map(|h| &h[..8])
            .ok_or_else(|| RefError("embedded reference has no hash to compile to".into()))
    }

    /// What the tag becomes in compiled output when refs are not revealed.
    pub fn compile(&self, embed: bool) -> Result<String, RefError> {
        match self.type_name {
            // `plain` compiles to its data (and resolves sub-variables right away).
            RefType::Plain => match &self.embedded_subvar_path {
                Some(sub) => {
                    let text = if self.encoding == "base64" {
                        String::from_utf8(b64_decode(&self.data)?)
                            .map_err(|e| RefError(format!("plain reference is not UTF-8: {e}")))?
                    } else {
                        self.data.clone()
                    };
                    let value = subvar_value(&text, sub, "PlainRef")?;
                    Ok(if self.encoding == "base64" {
                        b64_encode(value.as_bytes())
                    } else {
                        value
                    })
                }
                None => Ok(self.data.clone()),
            },
            // `env` is revealed later from the environment, never embedded.
            RefType::Env => Ok(format!(
                "?{{env:{}:{}}}",
                self.path.as_deref().unwrap_or(""),
                self.short_hash()?
            )),
            _ if embed => Ok(self.compile_embedded()),
            t => Ok(format!(
                "?{{{t}:{}:{}}}",
                self.path.as_deref().unwrap_or(""),
                self.short_hash()?
            )),
        }
    }
}

/// `value` at dotted `path` inside the YAML text of a secret.
fn subvar_value(yaml_text: &str, path: &str, who: &str) -> Result<String, RefError> {
    let node = parse_document(yaml_text, SourceId::SYNTHETIC)
        .map_err(|e| RefError(format!("{who}: revealed secret is not valid YAML: {e}")))?;
    let Value::Map(_) = &node.value else {
        return Err(RefError(format!(
            "{who}: revealed secret is not in embedded yaml, cannot access sub-variable at {path}"
        )));
    };
    let mut cur = &node;
    for key in path.split('.') {
        cur = cur
            .get(key)
            .ok_or_else(|| RefError(format!("{who}: cannot access sub-variable key {path}")))?;
    }
    Ok(match &cur.value {
        Value::Str(s) => s.clone(),
        other => other.py_str(),
    })
}

/// What creating a missing ref needs from the target being compiled
/// (kapitan's `target_name` ref parameter).
#[derive(Clone, Debug, Default)]
pub struct TargetSecrets {
    pub target: Option<String>,
    /// `parameters.kapitan.secrets` of the target.
    pub secrets: Option<Json>,
}

impl TargetSecrets {
    pub fn from_document(target: &str, doc: &Json) -> TargetSecrets {
        TargetSecrets {
            target: Some(target.to_string()),
            secrets: doc.pointer("/parameters/kapitan/secrets").cloned(),
        }
    }

    pub fn section(&self, name: &str) -> Option<&Json> {
        self.secrets.as_ref()?.get(name).filter(|v| !v.is_null())
    }

    fn missing(&self, what: &str) -> RefError {
        match &self.target {
            Some(t) => RefError(format!(
                "parameters.kapitan.secrets.{what} not defined in target {t}"
            )),
            None => RefError(format!(
                "parameters.kapitan.secrets.{what} is needed to create this reference (no target given)"
            )),
        }
    }
}

/// Parsed `?{...}` tag.
struct Tag<'a> {
    /// `type:path[:more]`
    token: &'a str,
    /// `||func:a|func2` when present.
    funcs: Option<&'a str>,
}

fn parse_tag(tag: &str) -> Result<Tag<'_>, RefError> {
    let caps = tag_regex()
        .captures(tag)
        .filter(|c| c.get(1).unwrap().as_str().len() == tag.len())
        .ok_or_else(|| {
            RefError(format!(
                "{tag}: is not a valid tag; try something like: ?{{ref:path/to/secret||function:param1:param2}}"
            ))
        })?;
    Ok(Tag {
        token: caps.get(2).unwrap().as_str(),
        funcs: caps.get(3).map(|m| m.as_str()),
    })
}

/// Loading, creating, compiling and revealing refs under one refs path.
pub struct RefController {
    pub refs_path: PathBuf,
    pub embed: bool,
    /// Loaded ref files by path (`None` = missing).
    cache: Mutex<HashMap<String, Option<Ref>>>,
    /// Revealed plaintext by token (kapitan's `lru_cache` on `_reveal_tag_without_subvar`).
    revealed: Mutex<HashMap<String, String>>,
    /// One ref is created at a time (targets compile in parallel and may share refs).
    create_lock: Mutex<()>,
    gkms: gkms::Client,
    vault: vault::Clients,
}

impl RefController {
    pub fn new(refs_path: PathBuf, embed: bool) -> Self {
        RefController {
            refs_path,
            embed,
            cache: Mutex::new(HashMap::new()),
            revealed: Mutex::new(HashMap::new()),
            create_lock: Mutex::new(()),
            gkms: gkms::Client::new(),
            vault: vault::Clients::default(),
        }
    }

    // ---- compile -------------------------------------------------------

    /// Replace every tag in `text` with its compiled form, creating refs
    /// that have functions and do not exist yet. Ref files consulted are
    /// added to `reads`.
    pub fn compile_str(
        &self,
        text: &str,
        target: &TargetSecrets,
        reads: &mut Reads,
    ) -> Result<String, RefError> {
        if !text.contains("?{") {
            return Ok(text.to_string());
        }
        replace_tags(text, |tag| self.compile_tag(tag, target, reads))
    }

    /// Replace tags in every string of a value tree. Inside mappings the
    /// keys whose value needs `||reveal:` are retried after the others, so
    /// a reference may depend on one created by a sibling key (kapitan's
    /// multi-pass `compile_obj`).
    pub fn compile_value(
        &self,
        v: &mut Value,
        target: &TargetSecrets,
        reads: &mut Reads,
    ) -> Result<(), RefError> {
        match v {
            Value::Str(s) if s.contains("?{") => {
                *s = self.compile_str(s, target, reads)?;
            }
            Value::List(l) => {
                for n in l {
                    self.compile_value(&mut n.value, target, reads)?;
                }
            }
            Value::Map(m) => {
                let mut done = vec![false; m.len()];
                let mut pending = m.len();
                for _pass in 0..m.len() + 1 {
                    let mut progressed = false;
                    for (i, (_, node)) in m.iter_mut().enumerate() {
                        if done[i] {
                            continue;
                        }
                        let dependent =
                            matches!(&node.value, Value::Str(s) if s.contains("||reveal:"));
                        if dependent {
                            if self.compile_value(&mut node.value, target, reads).is_err() {
                                continue;
                            }
                        } else {
                            self.compile_value(&mut node.value, target, reads)?;
                        }
                        done[i] = true;
                        pending -= 1;
                        progressed = true;
                    }
                    if pending == 0 || !progressed {
                        break;
                    }
                }
                // Whatever still fails now is a real error.
                for (i, (_, node)) in m.iter_mut().enumerate() {
                    if !done[i] {
                        self.compile_value(&mut node.value, target, reads)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn compile_tag(
        &self,
        tag: &str,
        target: &TargetSecrets,
        reads: &mut Reads,
    ) -> Result<String, RefError> {
        let parsed = parse_tag(tag)?;
        let r = match self.lookup(&parsed, reads)? {
            Some(r) => r,
            None => match parsed.funcs {
                Some(funcs) => self.create(parsed.token, funcs, target, reads)?,
                None => return Err(self.not_found(parsed.token)),
            },
        };
        r.compile(self.embed)
    }

    fn not_found(&self, token: &str) -> RefError {
        let mut attrs = token.split(':');
        let type_name = attrs.next().unwrap_or("");
        let path = attrs.next().unwrap_or("");
        RefError(format!(
            "reference {type_name}:{path} not found under {} (run `krab refs --write {type_name}:{path} -f <file>` to create it)",
            self.refs_path.display()
        ))
    }

    // ---- reveal --------------------------------------------------------

    /// Replace every tag in `text` with the revealed secret.
    pub fn reveal_str(&self, text: &str, reads: &mut Reads) -> Result<String, RefError> {
        if !text.contains("?{") {
            return Ok(text.to_string());
        }
        replace_tags(text, |tag| self.reveal_tag(tag, reads))
    }

    /// Reveal tags in every string of a value tree.
    pub fn reveal_value(&self, v: &mut Value, reads: &mut Reads) -> Result<(), RefError> {
        match v {
            Value::Str(s) if s.contains("?{") => {
                *s = self.reveal_str(s, reads)?;
            }
            Value::List(l) => {
                for n in l {
                    self.reveal_value(&mut n.value, reads)?;
                }
            }
            Value::Map(m) => {
                for n in m.values_mut() {
                    self.reveal_value(&mut n.value, reads)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// kapitan's `Revealer._reveal_replace_match`.
    fn reveal_tag(&self, tag: &str, reads: &mut Reads) -> Result<String, RefError> {
        if let Some(m) = subvar_regex().find(tag) {
            // `?{type:path@a.b}`: reveal the secret, then pick the sub-variable.
            let subvar_path = &m.as_str()[1..];
            let tag_without = subvar_regex().replace(tag, "").to_string();
            let plaintext = self.reveal_plain(&tag_without, reads)?;
            let r = self.get(&tag_without, reads)?;
            let text = if r.encoding == "base64" {
                String::from_utf8(b64_decode(&plaintext)?)
                    .map_err(|e| RefError(format!("Revealer: secret is not UTF-8: {e}")))?
            } else {
                plaintext
            };
            let value = subvar_value(&text, subvar_path, "Revealer").map_err(|_| {
                RefError(format!(
                    "Revealer: cannot access {tag} sub-variable key {subvar_path}"
                ))
            })?;
            return Ok(if r.encoding == "base64" {
                b64_encode(value.as_bytes())
            } else {
                value
            });
        }
        let r = self.get(tag, reads)?;
        if let Some(sub) = &r.embedded_subvar_path {
            let revealed = self.reveal_ref(&r)?;
            return subvar_value(&revealed, sub, "Revealer").map_err(|_| {
                RefError(format!(
                    "Revealer: cannot access {tag} sub-variable key {sub}"
                ))
            });
        }
        self.reveal_plain(tag, reads)
    }

    /// Reveal a tag without sub-variable, remembering the result.
    fn reveal_plain(&self, tag: &str, reads: &mut Reads) -> Result<String, RefError> {
        if let Some(v) = self.revealed.lock().get(tag) {
            return Ok(v.clone());
        }
        let r = self.get(tag, reads)?;
        let out = self.reveal_ref(&r)?;
        self.revealed.lock().insert(tag.to_string(), out.clone());
        Ok(out)
    }

    /// The plaintext of a reference (kapitan's `reveal()` per backend).
    pub fn reveal_ref(&self, r: &Ref) -> Result<String, RefError> {
        let utf8 = |bytes: Vec<u8>, what: &str| {
            String::from_utf8(bytes)
                .map_err(|e| RefError(format!("{what}: revealed data is not UTF-8: {e}")))
        };
        match r.type_name {
            RefType::Plain => Ok(r.data.clone()),
            RefType::Base64 => {
                // kapitan returns the raw field when the bytes are not text.
                let decoded = b64_decode(&r.data)?;
                Ok(String::from_utf8(decoded).unwrap_or_else(|_| r.data.clone()))
            }
            RefType::Env => {
                let path = r.path.as_deref().ok_or_else(|| {
                    RefError("env reference without a path cannot be revealed".into())
                })?;
                let part = path.split('@').next().unwrap_or(path);
                let part = part.rsplit('/').next().unwrap_or(part);
                let var = format!("{ENV_REF_VAR_PREFIX}{part}");
                Ok(std::env::var(&var)
                    .or_else(|_| std::env::var(var.to_uppercase()))
                    .unwrap_or_else(|_| r.data.clone()))
            }
            RefType::Gkms => {
                let key = r.key.as_deref().unwrap_or("");
                let plain = self.gkms.decrypt(key, &b64_decode(&r.data)?)?;
                utf8(plain, "gkms")
            }
            RefType::AwsKms => {
                let key = r.key.as_deref().unwrap_or("");
                utf8(kms_cli::aws_decrypt(key, &b64_decode(&r.data)?)?, "awskms")
            }
            RefType::AzKms => {
                let key = r.key.as_deref().unwrap_or("");
                utf8(kms_cli::az_decrypt(key, &b64_decode(&r.data)?)?, "azkms")
            }
            RefType::Gpg => utf8(gpg::decrypt(&b64_decode(&r.data)?)?, "gpg"),
            RefType::VaultKv => {
                let params = r
                    .vault_params
                    .clone()
                    .unwrap_or_else(|| vault::normalize_params(None, RefType::VaultKv));
                let client = self.vault.get(&params, vault::skip_verify_explicit(None))?;
                let data = utf8(b64_decode(&r.data)?, "vaultkv")?;
                let (secret_path, secret_key) = data.split_once(':').ok_or_else(|| {
                    RefError(format!(
                        "Invalid vault secret: secret should be stored as 'path/in/vault:key', not '{data}'"
                    ))
                })?;
                let mount = vault::param_str(&params, "mount").unwrap_or_else(|| "secret".into());
                let engine = vault::param_str(&params, "engine").unwrap_or_else(|| "kv-v2".into());
                let value = client.kv_read_key(&engine, &mount, secret_path, secret_key)?;
                if r.encoding == "base64" {
                    utf8(b64_decode(&value)?, "vaultkv")
                } else {
                    Ok(value)
                }
            }
            RefType::VaultTransit => {
                let params = r
                    .vault_params
                    .clone()
                    .unwrap_or_else(|| vault::normalize_params(None, RefType::VaultTransit));
                let client = self.vault.get(&params, vault::skip_verify_explicit(None))?;
                let key = vault::param_str(&params, "crypto_key")
                    .ok_or_else(|| RefError("Cannot access vault params".into()))?;
                let mount = vault::param_str(&params, "mount").unwrap_or_else(|| "transit".into());
                let mut ciphertext = utf8(b64_decode(&r.data)?, "vaulttransit")?;
                if vault::param_bool(&params, "always_latest") {
                    ciphertext = client.transit_rewrap(&mount, &key, &ciphertext)?;
                }
                let plain_b64 = client.transit_decrypt(&mount, &key, &ciphertext)?;
                utf8(b64_decode(&plain_b64)?, "vaulttransit")
            }
        }
    }

    // ---- lookup --------------------------------------------------------

    /// kapitan's `ref_controller[tag]`: the reference a tag names, or an
    /// error when it does not exist (with the hint that a function would
    /// create it during compile).
    pub fn get(&self, tag: &str, reads: &mut Reads) -> Result<Ref, RefError> {
        let parsed = parse_tag(tag)?;
        match self.lookup(&parsed, reads)? {
            Some(r) => Ok(r),
            None => match parsed.funcs {
                Some(funcs) => Err(RefError(format!(
                    "{}: does not exist and must be created from function: {funcs}",
                    parsed.token
                ))),
                None => Err(self.not_found(parsed.token)),
            },
        }
    }

    /// `Some` when the ref exists (or is embedded), `None` when its file is
    /// missing; other problems (bad hash, unknown type) are errors.
    fn lookup(&self, tag: &Tag, reads: &mut Reads) -> Result<Option<Ref>, RefError> {
        if tag.funcs.is_some() && subvar_regex().is_match(tag.token) {
            return Err(RefError(
                "Ref: references with sub-variables must be created manually".into(),
            ));
        }
        self.get_from_token(tag.token, reads)
    }

    /// kapitan's `_get_from_token`.
    fn get_from_token(&self, token: &str, reads: &mut Reads) -> Result<Option<Ref>, RefError> {
        let attrs: Vec<&str> = token.split(':').collect();
        let type_name = RefType::parse(attrs[0])
            .ok_or_else(|| RefError(format!("no backend for ref type: {}", attrs[0])))?;
        match attrs.len() {
            // type:path
            2 => self.load(type_name, attrs[1], reads),
            // type:path:hash  or  type:payload:embedded
            3 => {
                if attrs[2] == "embedded" {
                    return Ok(Some(ref_from_embedded(type_name, attrs[1])?));
                }
                let Some(r) = self.load(type_name, attrs[1], reads)? else {
                    return Ok(None);
                };
                let stored = r.hash.as_deref().map(|h| &h[..8]).unwrap_or("");
                if stored == attrs[2] {
                    Ok(Some(r))
                } else {
                    Err(RefError(format!(
                        "{token}: token hash does not match with stored reference hash: {type_name}:{}:{stored}",
                        attrs[1]
                    )))
                }
            }
            // vaultkv: type:path:mount:path/in/vault:key
            5 => self.load(type_name, attrs[1], reads),
            _ => Ok(None),
        }
    }

    /// kapitan's `PlainRefBackend.__getitem__`: the ref at `ref_path`
    /// (which may carry `@subvar`), with its hash and token path set.
    fn load(
        &self,
        type_name: RefType,
        ref_path: &str,
        reads: &mut Reads,
    ) -> Result<Option<Ref>, RefError> {
        let file_path = subvar_regex().replace(ref_path, "").to_string();
        let full = self.refs_path.join(&file_path);
        reads.file(&full);
        let cached = self.cache.lock().get(&file_path).cloned();
        let loaded = match cached {
            Some(c) => c,
            None => {
                let loaded = match std::fs::read_to_string(&full) {
                    Ok(text) => Some(parse_ref_file(&text, &full)?),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                    Err(e) => {
                        return Err(RefError(format!("cannot read {}: {e}", full.display())));
                    }
                };
                self.cache.lock().insert(file_path.clone(), loaded.clone());
                loaded
            }
        };
        let Some(mut r) = loaded else {
            return Ok(None);
        };
        // kapitan loads the file through the tag's backend whatever `type`
        // the file says; the tag decides how it compiles and reveals.
        r.type_name = type_name;
        r.path = Some(ref_path.to_string());
        r.hash = Some(ref_hash(&file_path, &r.data));
        if let Some((_, sub)) = ref_path.split_once('@') {
            r.embedded_subvar_path = Some(sub.to_string());
        }
        Ok(Some(r))
    }

    /// The ref stored in a ref file anywhere on disk (`krab refs --ref-file`).
    pub fn ref_from_file(&self, path: &Path) -> Result<Ref, RefError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| RefError(format!("cannot read {}: {e}", path.display())))?;
        parse_ref_file(&text, path)
    }

    /// The ref stored in ref-file text read from elsewhere (stdin).
    pub fn ref_from_text(&self, text: &str) -> Result<Ref, RefError> {
        parse_ref_file(text, Path::new("<stdin>"))
    }

    // ---- create / write ------------------------------------------------

    /// Create the ref a `?{type:path||functions}` tag asks for, write it
    /// under the refs path and return it as loaded.
    fn create(
        &self,
        token: &str,
        funcs: &str,
        target: &TargetSecrets,
        reads: &mut Reads,
    ) -> Result<Ref, RefError> {
        let _guard = self.create_lock.lock();
        // Another target may have created it while we waited.
        if let Some(r) = self.get_from_token(token, reads)? {
            return Ok(r);
        }
        let attrs: Vec<&str> = token.split(':').collect();
        if !matches!(attrs.len(), 2 | 5) {
            return Err(RefError(format!("{token}: is not a valid token")));
        }
        let type_name = RefType::parse(attrs[0])
            .ok_or_else(|| RefError(format!("no backend for ref type: {}", attrs[0])))?;
        let path = attrs[1];
        let mut ctx = functions::FunctionContext::new(token);
        functions::eval_chain(&mut ctx, funcs, self, reads)?;
        let data = ctx.data.ok_or_else(|| {
            RefError(format!(
                "{token}: functions `{funcs}` generated no data; try something like ||random:str"
            ))
        })?;
        let (payload, encoding) = if ctx.encode_base64 {
            (b64_encode(data.as_bytes()), "base64")
        } else {
            (data, "original")
        };
        let r = self.from_params(type_name, payload.as_bytes(), encoding, token, target)?;
        self.write(path, &r)?;
        self.get_from_token(token, reads)?
            .ok_or_else(|| RefError(format!("{token}: written but cannot be read back")))
    }

    /// kapitan's `from_params` per backend: a new ref holding `payload`
    /// (already base64 text when `encoding` is `base64`), encrypted or
    /// stored where the type wants it.
    pub fn from_params(
        &self,
        type_name: RefType,
        payload: &[u8],
        encoding: &str,
        token: &str,
        target: &TargetSecrets,
    ) -> Result<Ref, RefError> {
        match type_name {
            RefType::Gkms | RefType::AwsKms | RefType::AzKms => {
                let key = target
                    .section(type_name.name())
                    .and_then(|s| s.get("key"))
                    .and_then(Json::as_str)
                    .ok_or_else(|| target.missing(&format!("{type_name}.key")))?;
                self.encrypt_kms(type_name, payload, encoding, key)
            }
            RefType::Gpg => {
                let recipients = target
                    .section("gpg")
                    .and_then(|s| s.get("recipients"))
                    .and_then(Json::as_array)
                    .ok_or_else(|| target.missing("gpg.recipients"))?;
                self.encrypt_gpg(payload, encoding, recipients)
            }
            RefType::VaultKv => {
                let params = vault::normalize_params(target.section("vaultkv"), RefType::VaultKv);
                let attrs: Vec<&str> = token.split(':').collect();
                if attrs.len() != 5 {
                    return Err(RefError(
                        "Could not create VaultSecret: ref token is invalid (expected ?{vaultkv:path:mount:path/in/vault:key||function})".into(),
                    ));
                }
                let mount = if attrs[2].is_empty() {
                    vault::param_str(&params, "mount").unwrap_or_else(|| "secret".into())
                } else {
                    attrs[2].to_string()
                };
                let path_in_vault = if attrs[3].is_empty() {
                    attrs[1]
                } else {
                    attrs[3]
                };
                if attrs[4].is_empty() {
                    return Err(RefError(
                        "Could not create VaultSecret: vaultkv: key is missing".into(),
                    ));
                }
                let explicit = vault::skip_verify_explicit(target.section("vaultkv"));
                self.write_vaultkv(
                    &params,
                    explicit,
                    payload,
                    encoding,
                    &mount,
                    path_in_vault,
                    attrs[4],
                )
            }
            RefType::VaultTransit => {
                let params =
                    vault::normalize_params(target.section("vaulttransit"), RefType::VaultTransit);
                let explicit = vault::skip_verify_explicit(target.section("vaulttransit"));
                self.encrypt_vaulttransit(&params, explicit, payload, encoding)
            }
            RefType::Base64 => Ok(Ref::new(RefType::Base64, b64_encode(payload), encoding)),
            RefType::Plain | RefType::Env => {
                let text = String::from_utf8(payload.to_vec()).map_err(|e| {
                    RefError(format!("{type_name} reference data is not UTF-8: {e}"))
                })?;
                Ok(Ref::new(type_name, text, encoding))
            }
        }
    }

    /// Encrypt `payload` with a KMS key into a new ref.
    pub fn encrypt_kms(
        &self,
        type_name: RefType,
        payload: &[u8],
        encoding: &str,
        key: &str,
    ) -> Result<Ref, RefError> {
        let ciphertext = match type_name {
            RefType::Gkms => self.gkms.encrypt(key, payload)?,
            RefType::AwsKms => kms_cli::aws_encrypt(key, payload)?,
            RefType::AzKms => kms_cli::az_encrypt(key, payload)?,
            other => return Err(RefError(format!("{other} is not a KMS backend"))),
        };
        let mut r = Ref::new(type_name, b64_encode(&ciphertext), encoding);
        r.key = Some(key.to_string());
        Ok(r)
    }

    /// Encrypt `payload` for GPG `recipients` (`[{name}|{fingerprint}]`).
    pub fn encrypt_gpg(
        &self,
        payload: &[u8],
        encoding: &str,
        recipients: &[Json],
    ) -> Result<Ref, RefError> {
        let fingerprints = gpg::lookup_fingerprints(recipients)?;
        if fingerprints.is_empty() {
            return Err(RefError(
                "No GPG recipients specified. Use --recipients or specify them in parameters.kapitan.secrets.gpg.recipients and use --target-name".into(),
            ));
        }
        let ciphertext = gpg::encrypt(payload, &fingerprints)?;
        let mut r = Ref::new(RefType::Gpg, b64_encode(&ciphertext), encoding);
        r.recipients = fingerprints;
        Ok(r)
    }

    /// Store `payload` as `key` of the Vault KV secret at `mount`/`path`.
    #[allow(clippy::too_many_arguments)]
    pub fn write_vaultkv(
        &self,
        params: &Json,
        skip_verify_explicit: bool,
        payload: &[u8],
        encoding: &str,
        mount: &str,
        path: &str,
        key: &str,
    ) -> Result<Ref, RefError> {
        let text = String::from_utf8(payload.to_vec())
            .map_err(|e| RefError(format!("vaultkv secret is not UTF-8: {e}")))?;
        let engine = vault::param_str(params, "engine").unwrap_or_else(|| "kv-v2".into());
        let client = self.vault.get(params, skip_verify_explicit)?;
        let mut secrets = client
            .kv_read(&engine, mount, path)?
            .unwrap_or(Json::Object(Default::default()));
        if let Json::Object(m) = &mut secrets {
            m.insert(key.to_string(), Json::String(text));
        }
        client.kv_write(&engine, mount, path, &secrets)?;
        let mut r = Ref::new(
            RefType::VaultKv,
            b64_encode(format!("{path}:{key}").as_bytes()),
            encoding,
        );
        r.vault_params = Some(params.clone());
        Ok(r)
    }

    /// Encrypt `payload` with the Vault transit key in `params`.
    pub fn encrypt_vaulttransit(
        &self,
        params: &Json,
        skip_verify_explicit: bool,
        payload: &[u8],
        encoding: &str,
    ) -> Result<Ref, RefError> {
        let key = vault::param_str(params, "crypto_key").ok_or_else(|| {
            RefError(
                "vaulttransit: crypto_key is not set in parameters.kapitan.secrets.vaulttransit"
                    .into(),
            )
        })?;
        let mount = vault::param_str(params, "mount").unwrap_or_else(|| "transit".into());
        let client = self.vault.get(params, skip_verify_explicit)?;
        let ciphertext = client.transit_encrypt(&mount, &key, &b64_encode(payload))?;
        let mut r = Ref::new(
            RefType::VaultTransit,
            b64_encode(ciphertext.as_bytes()),
            encoding,
        );
        r.vault_params = Some(params.clone());
        Ok(r)
    }

    /// kapitan's `backend[path] = ref`: write the ref file under the refs path.
    pub fn write(&self, ref_path: &str, r: &Ref) -> Result<PathBuf, RefError> {
        let full = self.refs_path.join(ref_path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| RefError(format!("cannot create {}: {e}", parent.display())))?;
        }
        std::fs::write(&full, r.to_yaml())
            .map_err(|e| RefError(format!("cannot write {}: {e}", full.display())))?;
        self.cache.lock().remove(ref_path);
        self.revealed.lock().clear();
        Ok(full)
    }

    /// Type name of a token (`gkms` of `gkms:path/x`).
    pub fn token_type(token: &str) -> &str {
        token.split(':').next().unwrap_or(token)
    }
}

fn replace_tags(
    text: &str,
    mut f: impl FnMut(&str) -> Result<String, RefError>,
) -> Result<String, RefError> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for caps in tag_regex().captures_iter(text) {
        let whole = caps.get(1).unwrap();
        out.push_str(&text[last..whole.start()]);
        out.push_str(&f(whole.as_str())?);
        last = whole.end();
    }
    out.push_str(&text[last..]);
    Ok(out)
}

/// `KAPITAN_VAR_<name>` holds the value of `?{env:.../name}` at reveal time.
pub const ENV_REF_VAR_PREFIX: &str = "KAPITAN_VAR_";

/// sha256 of the file path followed by the stored data.
pub fn ref_hash(file_path: &str, data: &str) -> String {
    let mut h = Sha256::new();
    h.update(file_path.as_bytes());
    h.update(data.as_bytes());
    hex::encode(h.finalize())
}

/// kapitan's `ref_from_embedded`: the JSON `dump()` of a ref, base64 encoded.
fn ref_from_embedded(type_name: RefType, payload: &str) -> Result<Ref, RefError> {
    let json = b64_decode(payload)?;
    let obj: Json = serde_json::from_slice(&json)
        .map_err(|e| RefError(format!("embedded reference payload is not JSON: {e}")))?;
    let mut r = ref_from_fields(type_name, &obj, "embedded reference")?;
    if let Some(sub) = obj.get("embedded_subvar_path").and_then(Json::as_str) {
        r.embedded_subvar_path = Some(sub.to_string());
    }
    Ok(r)
}

/// A ref from the fields of a ref file or embedded payload.
fn ref_from_fields(type_name: RefType, obj: &Json, what: &str) -> Result<Ref, RefError> {
    let data = match obj.get("data") {
        Some(Json::String(s)) => s.clone(),
        Some(other) if !other.is_null() && !other.is_object() && !other.is_array() => {
            other.to_string()
        }
        _ => return Err(RefError(format!("{what} has no `data`"))),
    };
    let encoding = obj
        .get("encoding")
        .and_then(Json::as_str)
        .unwrap_or("original");
    let mut r = Ref::new(type_name, data, encoding);
    if type_name.is_kms() {
        r.key = obj.get("key").and_then(Json::as_str).map(str::to_string);
    }
    if type_name == RefType::Gpg {
        let recipients = obj
            .get("recipients")
            .and_then(Json::as_array)
            .cloned()
            .unwrap_or_default();
        r.recipients = gpg::lookup_fingerprints(&recipients)?;
    }
    if type_name.is_vault() {
        r.vault_params = Some(vault::normalize_params(obj.get("vault_params"), type_name));
    }
    Ok(r)
}

/// Parse a ref file (a YAML mapping with at least `data` and `type`).
fn parse_ref_file(text: &str, path: &Path) -> Result<Ref, RefError> {
    let node = parse_document(text, SourceId::SYNTHETIC)
        .map_err(|e| RefError(format!("{}: {e}", path.display())))?;
    let Value::Map(_) = &node.value else {
        return Err(RefError(format!(
            "{}: reference file is not a mapping",
            path.display()
        )));
    };
    let obj = node.value.to_json();
    // Files written before kapitan recorded the type are gpg secrets.
    let type_name = match obj.get("type") {
        Some(Json::String(t)) => RefType::parse(t)
            .ok_or_else(|| RefError(format!("{}: unknown reference type `{t}`", path.display())))?,
        _ => RefType::Gpg,
    };
    ref_from_fields(
        type_name,
        &obj,
        &format!("{}: reference file", path.display()),
    )
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn temp_refs(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kapitan-refs-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    #[test]
    fn embeds_and_hashes() {
        let dir = temp_refs("embed");
        write(
            &dir,
            "targets/x/token",
            "data: c2VjcmV0\nencoding: original\nkey: projects/p/keyRings/k/cryptoKeys/c\ntype: gkms\n",
        );
        let mut reads = Reads::default();
        let ts = TargetSecrets::default();
        let rc = RefController::new(dir.clone(), true);
        let out = rc
            .compile_str("a ?{gkms:targets/x/token||random:str} b", &ts, &mut reads)
            .unwrap();
        let payload = out
            .trim_start_matches("a ?{gkms:")
            .trim_end_matches(":embedded} b");
        let json = String::from_utf8(b64_decode(payload).unwrap()).unwrap();
        assert_eq!(
            json,
            r#"{"data": "c2VjcmV0", "encoding": "original", "key": "projects/p/keyRings/k/cryptoKeys/c", "type": "gkms"}"#
        );
        let rc = RefController::new(dir.clone(), false);
        let out = rc
            .compile_str("?{gkms:targets/x/token}", &ts, &mut reads)
            .unwrap();
        let expected_hash = ref_hash("targets/x/token", "c2VjcmV0");
        assert_eq!(
            out,
            format!("?{{gkms:targets/x/token:{}}}", &expected_hash[..8])
        );
        // A tag carrying the right hash passes, a wrong one is an error.
        assert!(rc.compile_str(&out, &ts, &mut reads).is_ok());
        assert!(
            rc.compile_str("?{gkms:targets/x/token:00000000}", &ts, &mut reads)
                .unwrap_err()
                .0
                .contains("does not match")
        );
        assert!(
            rc.compile_str("?{gkms:targets/missing}", &ts, &mut reads)
                .is_err()
        );
        assert!(reads.files.iter().any(|f| f.ends_with("targets/x/token")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn plain_compiles_to_its_data_and_env_to_a_hash() {
        let dir = temp_refs("plain");
        write(
            &dir,
            "p/secret",
            "data: hello world\nencoding: original\ntype: plain\n",
        );
        write(
            &dir,
            "p/yaml",
            "data: \"a:\\n  b: nested\\n\"\nencoding: original\ntype: plain\n",
        );
        write(
            &dir,
            "e/var",
            "data: default\nencoding: original\ntype: env\n",
        );
        let rc = RefController::new(dir.clone(), true);
        let mut reads = Reads::default();
        let ts = TargetSecrets::default();
        assert_eq!(
            rc.compile_str("x=?{plain:p/secret}", &ts, &mut reads)
                .unwrap(),
            "x=hello world"
        );
        assert_eq!(
            rc.compile_str("?{plain:p/yaml@a.b}", &ts, &mut reads)
                .unwrap(),
            "nested"
        );
        let out = rc.compile_str("?{env:e/var}", &ts, &mut reads).unwrap();
        assert!(out.starts_with("?{env:e/var:") && out.len() == "?{env:e/var:".len() + 9);
        // Revealing env falls back to the stored default.
        assert_eq!(rc.reveal_str(&out, &mut reads).unwrap(), "default");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn creates_refs_from_functions() {
        let dir = temp_refs("create");
        let rc = RefController::new(dir.clone(), false);
        let mut reads = Reads::default();
        let ts = TargetSecrets::default();
        let out = rc
            .compile_str("?{base64:t/pw||random:str:12}", &ts, &mut reads)
            .unwrap();
        assert!(out.starts_with("?{base64:t/pw:"));
        let text = std::fs::read_to_string(dir.join("t/pw")).unwrap();
        assert!(text.starts_with("data: "), "{text}");
        assert!(
            text.ends_with("encoding: original\ntype: base64\n"),
            "{text}"
        );
        let revealed = rc.reveal_str(&out, &mut reads).unwrap();
        assert_eq!(revealed.len(), 12);
        // Second compile reuses the file (same hash).
        let again = rc
            .compile_str("?{base64:t/pw||random:str:12}", &ts, &mut reads)
            .unwrap();
        assert_eq!(out, again);

        // `|base64` stores the base64 of the plaintext and records the encoding.
        rc.compile_str("?{base64:t/b||random:int:4|base64}", &ts, &mut reads)
            .unwrap();
        let text = std::fs::read_to_string(dir.join("t/b")).unwrap();
        assert!(text.contains("encoding: base64\n"), "{text}");
        let r = rc.get("?{base64:t/b}", &mut reads).unwrap();
        let inner = String::from_utf8(b64_decode(&rc.reveal_ref(&r).unwrap()).unwrap()).unwrap();
        assert_eq!(inner.len(), 4);
        assert!(inner.chars().all(|c| c.is_ascii_digit()));

        // plain with a sha256 chain.
        rc.compile_str("?{plain:t/h||random:str|sha256:salt}", &ts, &mut reads)
            .unwrap();
        let r = rc.get("?{plain:t/h}", &mut reads).unwrap();
        assert_eq!(r.data.len(), 64);

        // Missing without functions stays an error; sub-variables cannot be created.
        assert!(rc.compile_str("?{base64:t/none}", &ts, &mut reads).is_err());
        assert!(
            rc.compile_str("?{base64:t/x@a||random:str}", &ts, &mut reads)
                .unwrap_err()
                .0
                .contains("created manually")
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reveal_dependencies_between_keys_resolve_in_any_order() {
        let dir = temp_refs("deps");
        let rc = RefController::new(dir.clone(), false);
        let mut reads = Reads::default();
        let ts = TargetSecrets::default();
        let mut doc = Map::new();
        doc.insert(
            "pub".into(),
            Node::synthetic(Value::Str(
                "?{base64:k/pub||reveal:k/priv|publickey}".into(),
            )),
        );
        doc.insert(
            "priv".into(),
            Node::synthetic(Value::Str("?{base64:k/priv||rsa:1024}".into())),
        );
        let mut v = Value::Map(doc);
        rc.compile_value(&mut v, &ts, &mut reads).unwrap();
        let m = v.as_map().unwrap();
        let pubkey = rc
            .reveal_str(m["pub"].as_str().unwrap(), &mut reads)
            .unwrap();
        assert!(
            pubkey.starts_with("-----BEGIN PUBLIC KEY-----\n"),
            "{pubkey}"
        );
        let privkey = rc
            .reveal_str(m["priv"].as_str().unwrap(), &mut reads)
            .unwrap();
        assert!(
            privkey.starts_with("-----BEGIN PRIVATE KEY-----\n"),
            "{privkey}"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reveals_embedded_and_subvars() {
        let dir = temp_refs("reveal");
        let yaml_b64 = b64_encode(b"user: admin\npass: s3cret\n");
        write(
            &dir,
            "t/creds",
            &format!("data: {yaml_b64}\nencoding: original\ntype: base64\n"),
        );
        let mut reads = Reads::default();
        let ts = TargetSecrets::default();
        let rc = RefController::new(dir.clone(), true);
        let embedded = rc
            .compile_str("?{base64:t/creds@pass}", &ts, &mut reads)
            .unwrap();
        assert!(embedded.ends_with(":embedded}"));
        assert_eq!(rc.reveal_str(&embedded, &mut reads).unwrap(), "s3cret");
        assert_eq!(
            rc.reveal_str("?{base64:t/creds@user}", &mut reads).unwrap(),
            "admin"
        );
        assert_eq!(
            rc.reveal_str("pw=?{base64:t/creds}", &mut reads).unwrap(),
            "pw=user: admin\npass: s3cret\n"
        );
        let rc = RefController::new(dir.clone(), false);
        let hashed = rc
            .compile_str("?{base64:t/creds}", &ts, &mut reads)
            .unwrap();
        assert_eq!(
            rc.reveal_str(&hashed, &mut reads).unwrap(),
            "user: admin\npass: s3cret\n"
        );
        // gkms with kapitan's `mock` key reveals to "mock".
        write(
            &dir,
            "t/mock",
            "data: bW9jaw==\nencoding: original\nkey: mock\ntype: gkms\n",
        );
        assert_eq!(rc.reveal_str("?{gkms:t/mock}", &mut reads).unwrap(), "mock");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn ref_file_format_matches_pyyaml() {
        let mut r = Ref::new(RefType::Gpg, "AAAA".into(), "original");
        r.recipients = vec!["ABCDEF".into()];
        assert_eq!(
            r.to_yaml(),
            "data: AAAA\nencoding: original\nrecipients:\n- fingerprint: ABCDEF\ntype: gpg\n"
        );
        let mut r = Ref::new(RefType::Gkms, "AAAA".into(), "base64");
        r.key = Some("projects/p/locations/global/keyRings/k/cryptoKeys/c".into());
        assert_eq!(
            r.to_yaml(),
            "data: AAAA\nencoding: base64\nkey: projects/p/locations/global/keyRings/k/cryptoKeys/c\ntype: gkms\n"
        );
        let r = Ref::new(RefType::Plain, "line1\nline2\n".into(), "original");
        assert_eq!(
            r.to_yaml(),
            "data: 'line1\n\n  line2\n\n  '\nencoding: original\ntype: plain\n"
        );
    }
}

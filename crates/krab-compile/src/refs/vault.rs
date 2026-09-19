//! HashiCorp Vault refs (`vaultkv`, `vaulttransit`) over Vault's HTTP API.
//! Connection and authentication follow kapitan's `vault_resources.py`:
//! `vault_params` from the inventory, filled in from the `VAULT_*`
//! environment variables, with token, approle, userpass, ldap and github
//! authentication.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::{Map as JsonMap, Value as Json, json};

use super::{RefError, RefType};

/// kapitan's `KapitanReferenceVaultCommon` fields, in declaration order,
/// with the environment variable that fills each in when the inventory
/// does not.
const FIELDS: [(&str, Option<&str>); 13] = [
    ("addr", Some("VAULT_ADDR")),
    ("skip_verify", Some("VAULT_SKIP_VERIFY")),
    ("client_key", Some("VAULT_CLIENT_KEY")),
    ("client_cert", Some("VAULT_CLIENT_CERT")),
    ("cacert", Some("VAULT_CACERT")),
    ("capath", Some("VAULT_CAPATH")),
    ("namespace", Some("VAULT_NAMESPACE")),
    ("engine", None),
    ("auth", None),
    ("crypto_key", None),
    ("always_latest", None),
    ("mount", None),
    ("key", None),
];

/// The full `vault_params` object kapitan would build from `raw`
/// (`parameters.kapitan.secrets.vaultkv|vaulttransit` or a ref file's
/// `vault_params`), every field present.
pub fn normalize_params(raw: Option<&Json>, type_name: RefType) -> Json {
    let mut out = JsonMap::new();
    let given = |k: &str| raw.and_then(|r| r.get(k)).filter(|v| !v.is_null()).cloned();
    for (name, env) in FIELDS {
        let value = given(name).or_else(|| match name {
            "skip_verify" => Some(
                env.and_then(|e| std::env::var(e).ok())
                    .map(|v| Json::Bool(matches!(v.to_lowercase().as_str(), "1" | "true" | "yes")))
                    .unwrap_or(Json::Bool(true)),
            ),
            "engine" => Some(Json::String(
                match type_name {
                    RefType::VaultTransit => "transit",
                    _ => "kv-v2",
                }
                .into(),
            )),
            "always_latest" => Some(Json::Bool(false)),
            "mount" => Some(Json::String(
                match type_name {
                    RefType::VaultTransit => "transit",
                    _ => "secret",
                }
                .into(),
            )),
            _ => env
                .and_then(|e| std::env::var(e).ok())
                .filter(|v| !v.is_empty())
                .map(Json::String),
        });
        out.insert(name.to_string(), value.unwrap_or(Json::Null));
    }
    Json::Object(out)
}

pub fn param_str(params: &Json, key: &str) -> Option<String> {
    match params.get(key)? {
        Json::String(s) => Some(s.clone()),
        Json::Null => None,
        other => Some(other.to_string()),
    }
}

pub fn param_bool(params: &Json, key: &str) -> bool {
    match params.get(key) {
        Some(Json::Bool(b)) => *b,
        Some(Json::String(s)) => matches!(s.to_lowercase().as_str(), "1" | "true" | "yes"),
        _ => false,
    }
}

/// One authenticated client per distinct `vault_params`.
#[derive(Default)]
pub struct Clients {
    clients: Mutex<HashMap<String, Arc<VaultClient>>>,
}

impl Clients {
    pub fn get(&self, params: &Json) -> Result<Arc<VaultClient>, RefError> {
        let key = params.to_string();
        if let Some(c) = self.clients.lock().get(&key) {
            return Ok(c.clone());
        }
        let client = Arc::new(VaultClient::connect(params)?);
        self.clients.lock().insert(key, client.clone());
        Ok(client)
    }
}

pub struct VaultClient {
    agent: ureq::Agent,
    base: String,
    token: String,
    namespace: Option<String>,
}

fn read_token_file() -> Result<String, RefError> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::path::Path::new(&home).join(".vault-token");
    if path.is_symlink() {
        return Err(RefError(format!(
            "Token file {} is a symbolic link and will not be read",
            path.display()
        )));
    }
    let token = std::fs::read_to_string(&path)
        .map_err(|_| RefError(format!("Cannot read file {}", path.display())))?;
    if token.trim().is_empty() {
        return Err(RefError(format!("{} is empty", path.display())));
    }
    Ok(token.trim().to_string())
}

impl VaultClient {
    pub fn connect(params: &Json) -> Result<VaultClient, RefError> {
        let base = param_str(params, "addr")
            .filter(|a| !a.is_empty())
            .ok_or_else(|| {
                RefError(
                    "vault: no address; set parameters.kapitan.secrets.vault*.addr or VAULT_ADDR"
                        .into(),
                )
            })?;
        let mut tls = ureq::tls::TlsConfig::builder();
        if param_bool(params, "skip_verify") {
            tls = tls.disable_verification(true);
        } else {
            let bundle = param_str(params, "cacert")
                .filter(|s| !s.is_empty())
                .or_else(|| param_str(params, "capath").filter(|s| !s.is_empty()))
                .ok_or_else(|| {
                    RefError("Neither VAULT_CACERT nor VAULT_CAPATH specified".into())
                })?;
            let mut pem = Vec::new();
            let bundle_path = std::path::Path::new(&bundle);
            if bundle_path.is_dir() {
                let mut entries: Vec<_> = std::fs::read_dir(bundle_path)
                    .map_err(|e| RefError(format!("cannot read {bundle}: {e}")))?
                    .flatten()
                    .map(|e| e.path())
                    .collect();
                entries.sort();
                for p in entries {
                    if let Ok(mut bytes) = std::fs::read(&p) {
                        pem.append(&mut bytes);
                        pem.push(b'\n');
                    }
                }
            } else {
                pem = std::fs::read(bundle_path)
                    .map_err(|e| RefError(format!("cannot read CA bundle {bundle}: {e}")))?;
            }
            let certs: Vec<ureq::tls::Certificate<'static>> = ureq::tls::parse_pem(&pem)
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
                    _ => None,
                })
                .collect();
            if certs.is_empty() {
                return Err(RefError(format!("CA bundle {bundle} holds no certificate")));
            }
            tls = tls.root_certs(ureq::tls::RootCerts::new_with_certs(&certs));
        }
        let client_cert = param_str(params, "client_cert").filter(|s| !s.is_empty());
        let client_key = param_str(params, "client_key").filter(|s| !s.is_empty());
        if let (Some(cert_path), Some(key_path)) = (client_cert, client_key) {
            let cert_pem = std::fs::read(&cert_path).map_err(|e| {
                RefError(format!("cannot read client certificate {cert_path}: {e}"))
            })?;
            let key_pem = std::fs::read(&key_path)
                .map_err(|e| RefError(format!("cannot read client key {key_path}: {e}")))?;
            let certs: Vec<ureq::tls::Certificate<'static>> = ureq::tls::parse_pem(&cert_pem)
                .filter_map(|item| match item {
                    Ok(ureq::tls::PemItem::Certificate(c)) => Some(c),
                    _ => None,
                })
                .collect();
            let key = ureq::tls::parse_pem(&key_pem)
                .find_map(|item| match item {
                    Ok(ureq::tls::PemItem::PrivateKey(k)) => Some(k),
                    _ => None,
                })
                .ok_or_else(|| RefError(format!("{key_path} holds no private key")))?;
            tls = tls.client_cert(Some(ureq::tls::ClientCert::new_with_certs(&certs, key)));
        }
        let agent = ureq::Agent::config_builder()
            .tls_config(tls.build())
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(60)))
            .user_agent(format!("kapitan/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .new_agent();
        let mut client = VaultClient {
            agent,
            base: base.trim_end_matches('/').to_string(),
            token: String::new(),
            namespace: param_str(params, "namespace").filter(|s| !s.is_empty()),
        };
        client.authenticate(params)?;
        Ok(client)
    }

    fn authenticate(&mut self, params: &Json) -> Result<(), RefError> {
        let auth = param_str(params, "auth").unwrap_or_default();
        let env_token = || -> Result<String, RefError> {
            match std::env::var("VAULT_TOKEN") {
                Ok(t) if !t.is_empty() => Ok(t),
                _ => read_token_file(),
            }
        };
        let env = |k: &str| std::env::var(k).unwrap_or_default();
        let login = |client: &VaultClient, path: &str, body: Json| -> Result<String, RefError> {
            let (status, json) = client.request("POST", path, Some(&body))?;
            if !(200..300).contains(&status) {
                return Err(RefError(format!(
                    "vault: login at {path} failed: HTTP {status} {}",
                    errors_of(&json)
                )));
            }
            json.pointer("/auth/client_token")
                .and_then(Json::as_str)
                .map(str::to_string)
                .ok_or_else(|| RefError(format!("vault: login at {path} returned no client_token")))
        };
        self.token = match auth.as_str() {
            "token" => env_token()?,
            "github" => {
                let token = env_token()?;
                login(self, "auth/github/login", json!({ "token": token }))?
            }
            "ldap" => login(
                self,
                &format!("auth/ldap/login/{}", env("VAULT_USERNAME")),
                json!({ "password": env("VAULT_PASSWORD") }),
            )?,
            "userpass" => login(
                self,
                &format!("auth/userpass/login/{}", env("VAULT_USERNAME")),
                json!({ "password": env("VAULT_PASSWORD") }),
            )?,
            "approle" => login(
                self,
                "auth/approle/login",
                json!({ "role_id": env("VAULT_ROLE_ID"), "secret_id": env("VAULT_SECRET_ID") }),
            )?,
            other => {
                return Err(RefError(format!(
                    "Authentication type '{}' not supported",
                    if other.is_empty() { "None" } else { other }
                )));
            }
        };
        let (status, _) = self.request("GET", "auth/token/lookup-self", None)?;
        if status != 200 {
            return Err(RefError(
                "Vault Authentication Error, check if token in env:VAULT_TOKEN is valid and not expired".into(),
            ));
        }
        Ok(())
    }

    fn headers<B>(&self, mut req: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        if !self.token.is_empty() {
            req = req.header("X-Vault-Token", &self.token);
        }
        if let Some(ns) = &self.namespace {
            req = req.header("X-Vault-Namespace", ns);
        }
        req
    }

    /// One call to `/v1/<path>`; the status and the JSON body (empty object
    /// when there is none).
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Json>,
    ) -> Result<(u16, Json), RefError> {
        let url = format!("{}/v1/{path}", self.base);
        let mut resp = match body {
            Some(b) => self.headers(self.agent.post(&url)).send_json(b),
            None => self.headers(self.agent.get(&url)).call(),
        }
        .map_err(|e| RefError(format!("vault: {method} {url}: {e}")))?;
        let status = resp.status().as_u16();
        let text = resp
            .body_mut()
            .read_to_string()
            .map_err(|e| RefError(format!("vault: {method} {url}: {e}")))?;
        let json = if text.trim().is_empty() {
            Json::Object(Default::default())
        } else {
            serde_json::from_str(&text).unwrap_or(Json::String(text))
        };
        Ok((status, json))
    }

    /// The secret at `mount`/`path`, `None` when the path does not exist.
    pub fn kv_read(&self, engine: &str, mount: &str, path: &str) -> Result<Option<Json>, RefError> {
        let (url, pointer) = if engine == "kv" {
            (format!("{mount}/{path}"), "/data")
        } else {
            (format!("{mount}/data/{path}"), "/data/data")
        };
        let (status, json) = self.request("GET", &url, None)?;
        match status {
            200 => Ok(json.pointer(pointer).cloned()),
            404 => Ok(None),
            403 => Err(RefError(format!(
                "Permission Denied. make sure the token is authorised to access '{path}' on Vault"
            ))),
            other => Err(RefError(format!(
                "vault: reading {url}: HTTP {other} {}",
                errors_of(&json)
            ))),
        }
    }

    /// kapitan's `VaultSecret._decrypt`: `key` of the secret at `path`.
    pub fn kv_read_key(
        &self,
        engine: &str,
        mount: &str,
        path: &str,
        key: &str,
    ) -> Result<String, RefError> {
        let secret = self
            .kv_read(engine, mount, path)?
            .ok_or_else(|| RefError(format!("path '{path}' does not exist on Vault")))?;
        let value = secret
            .get(key)
            .ok_or_else(|| RefError(format!("key '{key}' does not exist on Vault")))?;
        let text = match value {
            Json::String(s) => s.clone(),
            Json::Null => String::new(),
            other => other.to_string(),
        };
        if text.is_empty() {
            return Err(RefError(format!("'{key}' doesn't exist on '{path}'")));
        }
        Ok(text)
    }

    pub fn kv_write(
        &self,
        engine: &str,
        mount: &str,
        path: &str,
        data: &Json,
    ) -> Result<(), RefError> {
        let (url, body) = if engine == "kv" {
            (format!("{mount}/{path}"), data.clone())
        } else {
            (format!("{mount}/data/{path}"), json!({ "data": data }))
        };
        let (status, json) = self.request("POST", &url, Some(&body))?;
        match status {
            200 | 204 => Ok(()),
            403 => Err(RefError(format!(
                "Permission Denied. make sure the token is authorised to access '{path}' on Vault"
            ))),
            other => Err(RefError(format!(
                "vault: writing {url}: HTTP {other} {}",
                errors_of(&json)
            ))),
        }
    }

    fn transit(
        &self,
        op: &str,
        mount: &str,
        key: &str,
        body: Json,
        field: &str,
    ) -> Result<String, RefError> {
        let url = format!("{mount}/{op}/{key}");
        let (status, json) = self.request("POST", &url, Some(&body))?;
        match status {
            200 => json
                .pointer(&format!("/data/{field}"))
                .and_then(Json::as_str)
                .map(str::to_string)
                .ok_or_else(|| RefError(format!("vault: {url}: response has no {field}"))),
            403 => Err(RefError(format!(
                "Permission Denied. make sure the token is authorised to access {key} on Vault"
            ))),
            404 => Err(RefError(format!("{key} does not exist on Vault secret"))),
            other => Err(RefError(format!(
                "vault: {url}: HTTP {other} {}",
                errors_of(&json)
            ))),
        }
    }

    /// `vault:v1:...` ciphertext of base64 `plaintext_b64`.
    pub fn transit_encrypt(
        &self,
        mount: &str,
        key: &str,
        plaintext_b64: &str,
    ) -> Result<String, RefError> {
        self.transit(
            "encrypt",
            mount,
            key,
            json!({ "plaintext": plaintext_b64 }),
            "ciphertext",
        )
    }

    /// Base64 plaintext of `ciphertext`.
    pub fn transit_decrypt(
        &self,
        mount: &str,
        key: &str,
        ciphertext: &str,
    ) -> Result<String, RefError> {
        self.transit(
            "decrypt",
            mount,
            key,
            json!({ "ciphertext": ciphertext }),
            "plaintext",
        )
    }

    pub fn transit_rewrap(
        &self,
        mount: &str,
        key: &str,
        ciphertext: &str,
    ) -> Result<String, RefError> {
        self.transit(
            "rewrap",
            mount,
            key,
            json!({ "ciphertext": ciphertext }),
            "ciphertext",
        )
    }
}

fn errors_of(json: &Json) -> String {
    json.get("errors")
        .and_then(Json::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Json::as_str)
                .collect::<Vec<_>>()
                .join("; ")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inputs::Reads;
    use crate::refs::{RefController, TargetSecrets, b64_decode};
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;

    /// A tiny Vault look-alike: token auth, KV v2 under `secret/`, a
    /// `transit/` engine whose ciphertext is the base64 plaintext reversed.
    fn mock_vault(token: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            let mut store: HashMap<String, Json> = HashMap::new();
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap() == 0 {
                    continue;
                }
                let mut parts = line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut len = 0;
                let mut got_token = String::new();
                loop {
                    let mut h = String::new();
                    reader.read_line(&mut h).unwrap();
                    let h = h.trim_end().to_string();
                    if h.is_empty() {
                        break;
                    }
                    let lower = h.to_lowercase();
                    if let Some(v) = lower.strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap();
                    }
                    if let Some(v) = lower.strip_prefix("x-vault-token:") {
                        got_token = v.trim().to_string();
                    }
                }
                let mut body = vec![0u8; len];
                reader.read_exact(&mut body).unwrap();
                let body: Json = serde_json::from_slice(&body).unwrap_or(Json::Null);
                let (status, resp) = if got_token != token {
                    (403, json!({"errors": ["permission denied"]}))
                } else if path == "/v1/auth/token/lookup-self" {
                    (200, json!({"data": {"id": token}}))
                } else if let Some(p) = path.strip_prefix("/v1/secret/data/") {
                    if method == "GET" {
                        match store.get(p) {
                            Some(d) => (200, json!({"data": {"data": d}})),
                            None => (404, json!({"errors": []})),
                        }
                    } else {
                        store.insert(p.to_string(), body["data"].clone());
                        (200, json!({"data": {"version": 1}}))
                    }
                } else if path == "/v1/transit/encrypt/k1" {
                    let p = body["plaintext"].as_str().unwrap();
                    (
                        200,
                        json!({"data": {"ciphertext": format!("vault:v1:{}", p.chars().rev().collect::<String>())}}),
                    )
                } else if path == "/v1/transit/decrypt/k1" {
                    let c = body["ciphertext"]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("vault:v1:");
                    (
                        200,
                        json!({"data": {"plaintext": c.chars().rev().collect::<String>()}}),
                    )
                } else if path == "/v1/transit/rewrap/k1" {
                    (200, json!({"data": {"ciphertext": body["ciphertext"]}}))
                } else {
                    (404, json!({"errors": ["no handler"]}))
                };
                let text = resp.to_string();
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                    text.len()
                );
            }
        });
        addr
    }

    #[test]
    fn normalizes_params_like_kapitans_model() {
        let p = normalize_params(
            Some(&json!({"auth": "token", "addr": "http://v"})),
            RefType::VaultKv,
        );
        let keys: Vec<&str> = p.as_object().unwrap().keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            [
                "addr",
                "skip_verify",
                "client_key",
                "client_cert",
                "cacert",
                "capath",
                "namespace",
                "engine",
                "auth",
                "crypto_key",
                "always_latest",
                "mount",
                "key"
            ]
        );
        assert_eq!(p["engine"], "kv-v2");
        assert_eq!(p["mount"], "secret");
        assert_eq!(p["skip_verify"], true);
        assert_eq!(p["key"], Json::Null);
        let p = normalize_params(None, RefType::VaultTransit);
        assert_eq!(p["engine"], "transit");
        assert_eq!(p["mount"], "transit");
    }

    #[test]
    fn kv_and_transit_round_trip_against_a_mock_server() {
        let addr = mock_vault("s.token");
        // SAFETY: the test process sets this once, before any client connects.
        unsafe { std::env::set_var("VAULT_TOKEN", "s.token") };
        let dir = crate::refs::tests::temp_refs("vault");
        let rc = RefController::new(dir.clone(), false);
        let mut reads = Reads::default();
        let ts = TargetSecrets {
            target: Some("t".into()),
            secrets: Some(json!({
                "vaultkv": {"auth": "token", "addr": addr, "engine": "kv-v2", "mount": "secret"},
                "vaulttransit": {"auth": "token", "addr": addr, "crypto_key": "k1"},
            })),
        };
        // vaultkv: created from a function, stored in the mock, revealed back.
        let out = rc
            .compile_str(
                "?{vaultkv:t/db/pw:secret:app/db:password||random:str:10}",
                &ts,
                &mut reads,
            )
            .unwrap();
        assert!(out.starts_with("?{vaultkv:t/db/pw:"), "{out}");
        let file = std::fs::read_to_string(dir.join("t/db/pw")).unwrap();
        assert!(
            file.contains("type: vaultkv\nvault_params:\n  addr: "),
            "{file}"
        );
        let r = rc.get("?{vaultkv:t/db/pw}", &mut reads).unwrap();
        assert_eq!(b64_decode(&r.data).unwrap(), b"app/db:password");
        let revealed = rc.reveal_str(&out, &mut reads).unwrap();
        assert_eq!(revealed.len(), 10);
        // A second key in the same secret keeps the first.
        rc.compile_str(
            "?{vaultkv:t/db/user:secret:app/db:username||random:loweralpha:6}",
            &ts,
            &mut reads,
        )
        .unwrap();
        assert_eq!(
            rc.reveal_str("?{vaultkv:t/db/pw}", &mut reads).unwrap(),
            revealed
        );
        // A missing key in the token is refused, like kapitan.
        assert!(
            rc.compile_str(
                "?{vaultkv:t/db/x:secret:app/db:||random:str}",
                &ts,
                &mut reads
            )
            .unwrap_err()
            .0
            .contains("key is missing")
        );
        // vaulttransit round trip.
        let out = rc
            .compile_str("?{vaulttransit:t/tok||random:int:6}", &ts, &mut reads)
            .unwrap();
        let revealed = rc.reveal_str(&out, &mut reads).unwrap();
        assert_eq!(revealed.len(), 6);
        assert!(revealed.chars().all(|c| c.is_ascii_digit()));
        // Wrong token: authentication error.
        // SAFETY: the test process sets this once, before any client connects.
        unsafe { std::env::set_var("VAULT_TOKEN", "bad") };
        let rc2 = RefController::new(dir.clone(), false);
        let err = rc2.reveal_str(&out, &mut reads).unwrap_err().0;
        assert!(err.contains("Authentication Error"), "{err}");
        let _ = std::fs::remove_dir_all(dir);
    }
}

//! JSON-RPC 2.0 over a unix socket, one JSON document per line.
//!
//! Methods:
//! * `server.info` → [`InfoResult`]
//! * `server.shutdown`
//! * `inventory.targets` → [`TargetsResult`]
//! * `inventory.target` ([`TargetParams`]) → [`TargetResult`]
//! * `inventory.all` → [`AllResult`]
//! * `inventory.class_usage` → `Vec<ClassUsage>`
//! * `inventory.classes` ([`TargetParams`]) → `Vec<String>`
//! * `inventory.explain` ([`ExplainParams`]) → `Explanation`
//! * `inventory.deps` ([`DepsParams`]) → `Vec<String>`
//! * `inventory.diagnostics` → [`DiagnosticsResult`]
//! * `inventory.wait` ([`WaitParams`]) → [`WaitResult`] (long poll for changes)

use std::path::PathBuf;

use krab_inventory::Diagnostic;
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    pub id: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    /// One or more [`Diagnostic`]s when the failure came from the inventory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

pub const ERR_PARSE: i64 = -32700;
pub const ERR_METHOD: i64 = -32601;
pub const ERR_PARAMS: i64 = -32602;
pub const ERR_INVENTORY: i64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InfoResult {
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
    /// The binary running the server.
    pub exe: PathBuf,
    /// `false` while the initial render runs; `inventory.*` requests wait for it.
    pub ready: bool,
    /// Where the resolver registry came from (the native set, a `resolvers.py`).
    pub resolvers: String,
    pub inventory_path: PathBuf,
    pub socket: PathBuf,
    pub log: Option<PathBuf>,
    pub generation: u64,
    pub targets: usize,
    pub errors: usize,
    pub uptime_secs: u64,
    pub idle_timeout_secs: u64,
    pub last_change: Option<ChangeSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetParams {
    pub name: String,
    /// Optional path inside the target document (`parameters.a.b`).
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetResult {
    pub name: String,
    pub digest: String,
    pub generation: u64,
    pub document: serde_json::Value,
    #[serde(default)]
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetSummary {
    pub name: String,
    pub path: String,
    pub file: PathBuf,
    pub digest: Option<String>,
    #[serde(default)]
    pub doc_digest: Option<String>,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Diagnostic>,
    /// `parameters.kapitan.labels`.
    #[serde(default)]
    pub labels: std::collections::BTreeMap<String, String>,
    /// Number of classes the target includes.
    #[serde(default)]
    pub classes: usize,
    /// Compile input types with their counts, e.g. `kadet×2`.
    #[serde(default)]
    pub inputs: Vec<String>,
}

/// Optional filter for `inventory.targets` and `inventory.all`: only targets
/// whose `parameters.kapitan.labels` contain every given pair.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TargetsParams {
    #[serde(default)]
    pub labels: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetsResult {
    pub generation: u64,
    pub targets: Vec<TargetSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AllResult {
    pub generation: u64,
    pub documents: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplainParams {
    pub target: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DepsParams {
    pub files: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticsResult {
    pub generation: u64,
    pub errors: Vec<Diagnostic>,
    pub warnings: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitParams {
    /// Return as soon as the server's generation is greater than this.
    pub generation: u64,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeSummary {
    pub generation: u64,
    /// Unix time in milliseconds.
    pub at: u64,
    pub changed_files: Vec<PathBuf>,
    /// Targets that were re-rendered (successfully or not).
    pub rerendered: Vec<String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub errors: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitResult {
    pub generation: u64,
    pub timed_out: bool,
    /// Changes since the requested generation (most recent last, capped).
    pub changes: Vec<ChangeSummary>,
}

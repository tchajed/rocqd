use lsp_types::Diagnostic;
use serde::{Deserialize, Serialize};

// --- Compile ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileRequest {
    pub file: String,
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompileResponse {
    pub diagnostics: Vec<Diagnostic>,
    pub success: bool,
}

// --- Query ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryRequest {
    pub file: String,
    pub line: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueryResponse {
    pub response: String,
}

// --- Status ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub file: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusResponse {
    pub sessions: Vec<SessionInfo>,
}

// --- Shutdown ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownResponse {}

// --- Invalidate ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidateRequest {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvalidateResponse {}

/// Method names for the rocqd protocol.
pub mod methods {
    pub const COMPILE: &str = "compile";
    pub const QUERY: &str = "query";
    pub const STATUS: &str = "status";
    pub const SHUTDOWN: &str = "shutdown";
    pub const INVALIDATE: &str = "invalidate";
}

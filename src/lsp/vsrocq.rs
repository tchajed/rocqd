use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};

use anyhow::{bail, Context, Result};
use lsp_types::{Diagnostic, Position, Range};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::jsonrpc::{Message, Notification, Request};
use super::transport::LspTransport;

// --- VsRocq-specific types ---

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateHighlightsParams {
    #[serde(default)]
    pub prepared_range: Vec<Range>,
    #[serde(default)]
    pub processing_range: Vec<Range>,
    #[serde(default)]
    pub processed_range: Vec<Range>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockOnErrorParams {
    pub uri: String,
    pub range: Range,
    pub message: String,
}

/// Events from vsrocqtop that the session should handle.
#[derive(Debug, Clone)]
pub enum VsRocqEvent {
    Diagnostics {
        uri: String,
        diagnostics: Vec<Diagnostic>,
    },
    UpdateHighlights(UpdateHighlightsParams),
    BlockOnError(BlockOnErrorParams),
}

/// Client for communicating with a vsrocqtop child process over LSP.
/// Uses direct I/O — the caller must drive both reading and writing.
pub struct VsRocqClient {
    writer: LspTransport<tokio::io::Empty, ChildStdin>,
    reader: LspTransport<ChildStdout, tokio::io::Sink>,
    next_id: AtomicI64,
    child: Child,
}

impl VsRocqClient {
    /// Spawn a vsrocqtop child process.
    pub async fn spawn(vsrocqtop_path: Option<&str>) -> Result<Self> {
        let path = vsrocqtop_path.unwrap_or("vsrocqtop");
        let mut child = Command::new(path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {path}"))?;

        let stdin = child.stdin.take().context("failed to open stdin")?;
        let stdout = child.stdout.take().context("failed to open stdout")?;

        let writer = LspTransport::new(tokio::io::empty(), stdin);
        let reader = LspTransport::new(stdout, tokio::io::sink());

        Ok(Self {
            writer,
            reader,
            next_id: AtomicI64::new(1),
            child,
        })
    }

    fn next_id(&self) -> i64 {
        self.next_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Send a notification (no response expected).
    async fn notify(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notif = Notification::new(method, params);
        self.writer
            .send_message(&Message::Notification(notif))
            .await
    }

    /// Send a request (with id) but don't wait for response.
    async fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let id = self.next_id();
        let req = Request::new(id, method, params);
        self.writer.send_message(&Message::Request(req)).await
    }

    /// Read the next message from vsrocqtop.
    pub async fn recv(&mut self) -> Result<Message> {
        self.reader.recv_message().await
    }

    /// Read the next event (notification) from vsrocqtop, parsing it into a
    /// VsRocqEvent. Responses are logged and skipped.
    pub async fn recv_event(&mut self) -> Result<Option<VsRocqEvent>> {
        loop {
            let msg = self.recv().await?;
            match msg {
                Message::Notification(notif) => {
                    if let Some(event) = Self::parse_notification(notif) {
                        return Ok(Some(event));
                    }
                    // Unknown notification, skip
                }
                Message::Response(resp) => {
                    tracing::debug!("received response for id={}", resp.id);
                }
                Message::Request(req) => {
                    tracing::debug!("received server request: {}", req.method);
                }
            }
        }
    }

    fn parse_notification(notif: Notification) -> Option<VsRocqEvent> {
        match notif.method.as_str() {
            "textDocument/publishDiagnostics" => {
                let params = notif.params?;
                let uri = params.get("uri")?.as_str()?.to_string();
                let diags = params.get("diagnostics")?;
                let diagnostics: Vec<Diagnostic> =
                    serde_json::from_value(diags.clone()).unwrap_or_default();
                Some(VsRocqEvent::Diagnostics { uri, diagnostics })
            }
            "prover/updateHighlights" => {
                let params = notif.params?;
                let highlights: UpdateHighlightsParams =
                    serde_json::from_value(params).ok()?;
                Some(VsRocqEvent::UpdateHighlights(highlights))
            }
            "prover/blockOnError" => {
                let params = notif.params?;
                let block: BlockOnErrorParams =
                    serde_json::from_value(params).ok()?;
                Some(VsRocqEvent::BlockOnError(block))
            }
            _ => {
                tracing::trace!("ignoring notification: {}", notif.method);
                None
            }
        }
    }

    /// Perform the LSP initialize handshake.
    pub async fn initialize(&mut self, root_uri: Option<&str>) -> Result<()> {
        let params = json!({
            "processId": std::process::id(),
            "capabilities": {},
            "rootUri": root_uri,
            "initializationOptions": {
                "proof": {
                    "mode": 1,
                    "delegation": "None",
                    "block": true
                }
            }
        });

        self.send_request("initialize", Some(params)).await?;
        self.notify("initialized", Some(json!({}))).await?;
        Ok(())
    }

    /// Send textDocument/didOpen notification.
    pub async fn did_open(&mut self, uri: &str, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didOpen",
            Some(json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "rocq",
                    "version": 1,
                    "text": text
                }
            })),
        )
        .await
    }

    /// Send textDocument/didChange notification with full content sync.
    pub async fn did_change(&mut self, uri: &str, version: i32, text: &str) -> Result<()> {
        self.notify(
            "textDocument/didChange",
            Some(json!({
                "textDocument": {
                    "uri": uri,
                    "version": version
                },
                "contentChanges": [{
                    "text": text
                }]
            })),
        )
        .await
    }

    /// Send prover/interpretToEnd notification.
    pub async fn interpret_to_end(&mut self, uri: &str, version: i32) -> Result<()> {
        self.notify(
            "prover/interpretToEnd",
            Some(json!({
                "textDocument": {
                    "uri": uri,
                    "version": version
                }
            })),
        )
        .await
    }

    /// Send prover/check request and return the response.
    pub async fn check(&mut self, uri: &str, position: Position, pattern: &str) -> Result<String> {
        let id = self.next_id();
        let req = Request::new(
            id,
            "prover/check",
            Some(json!({
                "textDocument": { "uri": uri },
                "position": position,
                "pattern": pattern
            })),
        );
        self.writer.send_message(&Message::Request(req)).await?;

        // Read until we get the response with our id
        let id_str = id.to_string();
        loop {
            let msg = self.recv().await?;
            match msg {
                Message::Response(resp) if resp.id.to_string() == id_str => {
                    if let Some(err) = &resp.error {
                        bail!("check failed: {}", err.message);
                    }
                    return Ok(resp
                        .result
                        .map(|v| format_query_result(&v))
                        .unwrap_or_default());
                }
                _ => {} // skip other messages while waiting
            }
        }
    }

    /// Send prover/print request and return the response.
    pub async fn print(&mut self, uri: &str, position: Position, pattern: &str) -> Result<String> {
        let id = self.next_id();
        let req = Request::new(
            id,
            "prover/print",
            Some(json!({
                "textDocument": { "uri": uri },
                "position": position,
                "pattern": pattern
            })),
        );
        self.writer.send_message(&Message::Request(req)).await?;

        let id_str = id.to_string();
        loop {
            let msg = self.recv().await?;
            match msg {
                Message::Response(resp) if resp.id.to_string() == id_str => {
                    if let Some(err) = &resp.error {
                        bail!("print failed: {}", err.message);
                    }
                    return Ok(resp
                        .result
                        .map(|v| format_query_result(&v))
                        .unwrap_or_default());
                }
                _ => {}
            }
        }
    }

    /// Send prover/about request and return the response.
    pub async fn about(&mut self, uri: &str, position: Position, pattern: &str) -> Result<String> {
        let id = self.next_id();
        let req = Request::new(
            id,
            "prover/about",
            Some(json!({
                "textDocument": { "uri": uri },
                "position": position,
                "pattern": pattern
            })),
        );
        self.writer.send_message(&Message::Request(req)).await?;

        let id_str = id.to_string();
        loop {
            let msg = self.recv().await?;
            match msg {
                Message::Response(resp) if resp.id.to_string() == id_str => {
                    if let Some(err) = &resp.error {
                        bail!("about failed: {}", err.message);
                    }
                    return Ok(resp
                        .result
                        .map(|v| format_query_result(&v))
                        .unwrap_or_default());
                }
                _ => {}
            }
        }
    }

    /// Shut down the vsrocqtop process.
    pub async fn shutdown(&mut self) -> Result<()> {
        let _ = self.notify("exit", None).await;

        tokio::select! {
            _ = self.child.wait() => {},
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                let _ = self.child.kill().await;
            }
        }

        Ok(())
    }
}

/// Convert a file path to a file:// URI.
pub fn path_to_uri(path: &Path) -> String {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(path)
    };
    format!("file://{}", abs.display())
}

/// Format a query result value into a human-readable string.
fn format_query_result(value: &Value) -> String {
    if let Some(s) = value.as_str() {
        return s.to_string();
    }
    if let Some(s) = value.get("message").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    serde_json::to_string_pretty(value).unwrap_or_default()
}

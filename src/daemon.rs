use std::path::PathBuf;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal;

use crate::lsp::jsonrpc::{Id, Message, Response};
use crate::protocol::{self, methods};
use crate::session::FileSession;

/// Get the socket path for the daemon.
pub fn socket_path() -> PathBuf {
    if let Some(runtime_dir) = dirs::runtime_dir() {
        runtime_dir.join("rocqd.sock")
    } else {
        // Fallback: /tmp/rocqd-$UID.sock
        let uid = unsafe { libc::getuid() };
        PathBuf::from(format!("/tmp/rocqd-{uid}.sock"))
    }
}

/// Get the PID file path.
fn pid_path() -> PathBuf {
    let sock = socket_path();
    sock.with_extension("pid")
}

/// Run the daemon server.
pub async fn run() -> Result<()> {
    let sock_path = socket_path();

    // Clean up stale socket
    if sock_path.exists() {
        tracing::info!("removing stale socket at {}", sock_path.display());
        tokio::fs::remove_file(&sock_path).await?;
    }

    // Ensure parent directory exists
    if let Some(parent) = sock_path.parent() {
        tokio::fs::create_dir_all(parent).await.ok();
    }

    let listener = UnixListener::bind(&sock_path)
        .with_context(|| format!("binding to {}", sock_path.display()))?;
    tracing::info!("listening on {}", sock_path.display());

    // Write PID file
    let pid_path = pid_path();
    tokio::fs::write(&pid_path, std::process::id().to_string()).await?;

    let mut session: Option<FileSession> = None;
    let mut shutdown_requested = false;

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        if let Err(e) = handle_connection(stream, &mut session, &mut shutdown_requested).await {
                            tracing::error!("error handling connection: {e}");
                        }
                        if shutdown_requested {
                            break;
                        }
                    }
                    Err(e) => {
                        tracing::error!("accept error: {e}");
                    }
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("received SIGINT, shutting down");
                break;
            }
        }
    }

    // Clean up
    if let Some(s) = session.take() {
        let _ = s.shutdown().await;
    }
    let _ = tokio::fs::remove_file(&sock_path).await;
    let _ = tokio::fs::remove_file(&pid_path).await;
    tracing::info!("daemon shut down");

    Ok(())
}

/// Handle a single client connection. Each connection sends one JSON-RPC
/// request and receives one response.
async fn handle_connection(
    stream: tokio::net::UnixStream,
    session: &mut Option<FileSession>,
    shutdown_requested: &mut bool,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Read one line (our protocol uses newline-delimited JSON)
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let line = line.trim();
    if line.is_empty() {
        return Ok(());
    }

    let value: serde_json::Value =
        serde_json::from_str(line).context("parsing client request")?;
    let msg = Message::parse(value).context("parsing JSON-RPC message")?;

    let response = match msg {
        Message::Request(req) => {
            let id = req.id.clone();
            match req.method.as_str() {
                methods::COMPILE => handle_compile(id, req.params, session).await,
                methods::QUERY => handle_query(id, req.params, session).await,
                methods::STATUS => handle_status(id, session),
                methods::SHUTDOWN => {
                    *shutdown_requested = true;
                    handle_shutdown(id, session).await
                }
                methods::INVALIDATE => handle_invalidate(id, req.params, session).await,
                other => {
                    Response::error(id, -32601, format!("method not found: {other}"))
                }
            }
        }
        _ => Response::error(Id::Number(0), -32600, "expected a request"),
    };

    let response_json = serde_json::to_string(&response)?;
    writer.write_all(response_json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    Ok(())
}

async fn handle_compile(
    id: Id,
    params: Option<serde_json::Value>,
    session: &mut Option<FileSession>,
) -> Response {
    let req: protocol::CompileRequest = match params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
    {
        Some(r) => r,
        None => return Response::error(id, -32602, "invalid compile params"),
    };

    let file_path = PathBuf::from(&req.file);

    // Check if we can reuse the existing session
    let needs_new_session = match session {
        Some(s) => {
            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.clone());
            s.path != canonical
        }
        None => true,
    };

    if needs_new_session {
        // Shut down existing session if any
        if let Some(s) = session.take() {
            let _ = s.shutdown().await;
        }
        // Open new session
        match FileSession::open(&file_path).await {
            Ok(s) => *session = Some(s),
            Err(e) => return Response::error(id, -32000, format!("failed to open session: {e}")),
        }
    } else {
        // Recompile with existing session
        let s = session.as_mut().unwrap();
        if let Err(e) = s.recompile().await {
            return Response::error(id, -32000, format!("recompile failed: {e}"));
        }
    }

    // Wait for completion
    let s = session.as_mut().unwrap();
    match s.wait_for_completion(300).await {
        Ok(diagnostics) => {
            let resp = protocol::CompileResponse {
                diagnostics: diagnostics.to_vec(),
                success: !s.has_errors(),
            };
            Response::ok(id, serde_json::to_value(resp).unwrap())
        }
        Err(e) => Response::error(id, -32000, format!("compilation error: {e}")),
    }
}

async fn handle_query(
    id: Id,
    params: Option<serde_json::Value>,
    session: &mut Option<FileSession>,
) -> Response {
    let req: protocol::QueryRequest = match params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
    {
        Some(r) => r,
        None => return Response::error(id, -32602, "invalid query params"),
    };

    let s = match session.as_mut() {
        Some(s) => s,
        None => return Response::error(id, -32000, "no active session — compile first"),
    };

    // Parse query type from text (e.g., "Check nat." -> query_type="check", pattern="nat")
    let (query_type, pattern) = parse_query_text(&req.text);

    match s.query(&query_type, req.line, &pattern).await {
        Ok(response) => {
            let resp = protocol::QueryResponse { response };
            Response::ok(id, serde_json::to_value(resp).unwrap())
        }
        Err(e) => Response::error(id, -32000, format!("query failed: {e}")),
    }
}

fn handle_status(id: Id, session: &Option<FileSession>) -> Response {
    let sessions = match session {
        Some(s) => vec![protocol::SessionInfo {
            file: s.path.display().to_string(),
            status: format!("{:?}", s.status),
        }],
        None => vec![],
    };

    let resp = protocol::StatusResponse { sessions };
    Response::ok(id, serde_json::to_value(resp).unwrap())
}

async fn handle_shutdown(id: Id, session: &mut Option<FileSession>) -> Response {
    if let Some(s) = session.take() {
        let _ = s.shutdown().await;
    }
    Response::ok(
        id,
        serde_json::to_value(protocol::ShutdownResponse {}).unwrap(),
    )
}

async fn handle_invalidate(
    id: Id,
    params: Option<serde_json::Value>,
    session: &mut Option<FileSession>,
) -> Response {
    let req: protocol::InvalidateRequest = match params
        .as_ref()
        .and_then(|p| serde_json::from_value(p.clone()).ok())
    {
        Some(r) => r,
        None => return Response::error(id, -32602, "invalid invalidate params"),
    };

    let file_path = PathBuf::from(&req.file);
    let canonical = file_path
        .canonicalize()
        .unwrap_or_else(|_| file_path.clone());

    if let Some(s) = session.as_ref() {
        if s.path == canonical {
            if let Some(s) = session.take() {
                let _ = s.shutdown().await;
            }
        }
    }

    Response::ok(
        id,
        serde_json::to_value(protocol::InvalidateResponse {}).unwrap(),
    )
}

/// Parse a query text like "Check nat." into (query_type, pattern).
fn parse_query_text(text: &str) -> (String, String) {
    let text = text.trim();
    // Try to parse "Command pattern." format
    if let Some(rest) = text.strip_prefix("Check ") {
        return ("check".to_string(), rest.trim_end_matches('.').trim().to_string());
    }
    if let Some(rest) = text.strip_prefix("Print ") {
        return ("print".to_string(), rest.trim_end_matches('.').trim().to_string());
    }
    if let Some(rest) = text.strip_prefix("About ") {
        return ("about".to_string(), rest.trim_end_matches('.').trim().to_string());
    }
    // Default: treat whole text as a check query
    ("check".to_string(), text.trim_end_matches('.').trim().to_string())
}

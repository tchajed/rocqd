use anyhow::{bail, Context, Result};
use lsp_types::DiagnosticSeverity;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::daemon::socket_path;
use crate::lsp::jsonrpc::{Message, Request, Response};
use crate::protocol;

/// Send a JSON-RPC request to the daemon and return the response.
async fn send_request(method: &str, params: Option<serde_json::Value>) -> Result<Response> {
    let sock_path = socket_path();
    let stream = UnixStream::connect(&sock_path)
        .await
        .with_context(|| format!("connecting to daemon at {}", sock_path.display()))?;

    let (reader, mut writer) = stream.into_split();

    let req = Request::new(1i64, method, params);
    let msg = Message::Request(req);
    let json = serde_json::to_string(&msg)?;
    writer.write_all(json.as_bytes()).await?;
    writer.write_all(b"\n").await?;
    writer.flush().await?;

    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let value: serde_json::Value = serde_json::from_str(line.trim())?;
    let msg = Message::parse(value)?;
    match msg {
        Message::Response(resp) => Ok(resp),
        _ => bail!("expected response from daemon"),
    }
}

/// rocqd stop — send shutdown request.
pub async fn stop() -> Result<()> {
    let resp = send_request(protocol::methods::SHUTDOWN, Some(serde_json::json!({})))
        .await
        .context("failed to connect to daemon — is it running?")?;

    if let Some(err) = resp.error {
        bail!("shutdown failed: {}", err.message);
    }

    eprintln!("daemon stopped");
    Ok(())
}

/// rocqd compile <file> [flags...] — send compile request, print diagnostics.
pub async fn compile(file: &str, flags: &[String]) -> Result<()> {
    let req = protocol::CompileRequest {
        file: file.to_string(),
        flags: flags.to_vec(),
    };

    let resp = send_request(
        protocol::methods::COMPILE,
        Some(serde_json::to_value(&req)?),
    )
    .await
    .context("failed to connect to daemon — is it running?")?;

    if let Some(err) = resp.error {
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let compile_resp: protocol::CompileResponse = serde_json::from_value(
        resp.result.context("missing result in compile response")?,
    )?;

    // Print diagnostics to stderr
    for diag in &compile_resp.diagnostics {
        let severity = match diag.severity {
            Some(DiagnosticSeverity::ERROR) => "error",
            Some(DiagnosticSeverity::WARNING) => "warning",
            Some(DiagnosticSeverity::INFORMATION) => "info",
            Some(DiagnosticSeverity::HINT) => "hint",
            _ => "note",
        };

        let range = &diag.range;
        eprintln!(
            "{file}:{}:{}: {severity}: {}",
            range.start.line + 1,
            range.start.character + 1,
            diag.message
        );
    }

    if !compile_resp.success {
        std::process::exit(1);
    }

    Ok(())
}

/// rocqd query <file>:<line> <text> — send query request, print result.
pub async fn query(file_line: &str, text: &str) -> Result<()> {
    let (file, line) = parse_file_line(file_line)?;

    let req = protocol::QueryRequest {
        file: file.to_string(),
        line,
        text: text.to_string(),
    };

    let resp = send_request(
        protocol::methods::QUERY,
        Some(serde_json::to_value(&req)?),
    )
    .await
    .context("failed to connect to daemon — is it running?")?;

    if let Some(err) = resp.error {
        eprintln!("error: {}", err.message);
        std::process::exit(1);
    }

    let query_resp: protocol::QueryResponse = serde_json::from_value(
        resp.result.context("missing result in query response")?,
    )?;

    println!("{}", query_resp.response);
    Ok(())
}

/// rocqd status — print active sessions.
pub async fn status() -> Result<()> {
    let resp = send_request(protocol::methods::STATUS, Some(serde_json::json!({})))
        .await
        .context("failed to connect to daemon — is it running?")?;

    if let Some(err) = resp.error {
        bail!("status failed: {}", err.message);
    }

    let status_resp: protocol::StatusResponse = serde_json::from_value(
        resp.result.context("missing result in status response")?,
    )?;

    if status_resp.sessions.is_empty() {
        println!("no active sessions");
    } else {
        for session in &status_resp.sessions {
            println!("{}: {}", session.file, session.status);
        }
    }

    Ok(())
}

/// Parse "file.v:42" into (file, line).
fn parse_file_line(s: &str) -> Result<(&str, u32)> {
    let (file, line_str) = s
        .rsplit_once(':')
        .context("expected format file.v:line")?;
    let line: u32 = line_str.parse().context("invalid line number")?;
    Ok((file, line))
}

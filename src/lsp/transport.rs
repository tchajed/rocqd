use anyhow::{bail, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use super::jsonrpc::Message;

/// LSP transport layer implementing Content-Length framing over async
/// reader/writer pairs (typically child process stdin/stdout).
pub struct LspTransport<R, W> {
    reader: BufReader<R>,
    writer: W,
}

impl<R, W> LspTransport<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader: BufReader::new(reader),
            writer,
        }
    }

    /// Send a JSON-RPC message with Content-Length framing.
    pub async fn send_message(&mut self, msg: &Message) -> Result<()> {
        let body = serde_json::to_string(msg)?;
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(body.as_bytes()).await?;
        self.writer.flush().await?;
        tracing::trace!(body = %body, "sent message");
        Ok(())
    }

    /// Send a raw JSON value with Content-Length framing.
    pub async fn send_raw(&mut self, body: &str) -> Result<()> {
        let header = format!("Content-Length: {}\r\n\r\n", body.len());
        self.writer.write_all(header.as_bytes()).await?;
        self.writer.write_all(body.as_bytes()).await?;
        self.writer.flush().await?;
        Ok(())
    }

    /// Receive a JSON-RPC message by parsing Content-Length header and reading
    /// the body.
    pub async fn recv_message(&mut self) -> Result<Message> {
        let content_length = self.read_headers().await?;
        let mut body = vec![0u8; content_length];
        self.reader
            .read_exact(&mut body)
            .await
            .context("reading message body")?;
        let body_str = std::str::from_utf8(&body).context("message body is not valid UTF-8")?;
        tracing::trace!(body = %body_str, "received message");
        let value: serde_json::Value =
            serde_json::from_str(body_str).context("parsing message JSON")?;
        Message::parse(value).context("parsing JSON-RPC message")
    }

    /// Parse headers until the empty line, returning the Content-Length value.
    async fn read_headers(&mut self) -> Result<usize> {
        let mut content_length: Option<usize> = None;
        loop {
            let mut line = String::new();
            let bytes_read = self
                .reader
                .read_line(&mut line)
                .await
                .context("reading header line")?;
            if bytes_read == 0 {
                bail!("unexpected EOF while reading headers");
            }
            let line = line.trim_end_matches("\r\n").trim_end_matches('\n');
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                content_length = Some(value.parse().context("parsing Content-Length")?);
            }
            // Ignore other headers (e.g., Content-Type)
        }
        content_length.context("missing Content-Length header")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::jsonrpc::{Notification, Request, Response};
    use serde_json::json;

    #[tokio::test]
    async fn roundtrip_request() {
        let (client_r, server_w) = tokio::io::duplex(4096);
        let (server_r, client_w) = tokio::io::duplex(4096);

        let mut sender = LspTransport::new(client_r, client_w);
        let mut receiver = LspTransport::new(server_r, server_w);

        let req = Message::Request(Request::new(
            1i64,
            "test/method",
            Some(json!({"key": "value"})),
        ));

        sender.send_message(&req).await.unwrap();
        let received = receiver.recv_message().await.unwrap();

        match received {
            Message::Request(r) => {
                assert_eq!(r.method, "test/method");
                assert_eq!(r.params.unwrap()["key"], "value");
            }
            other => panic!("expected Request, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn roundtrip_response() {
        let (client_r, server_w) = tokio::io::duplex(4096);
        let (server_r, client_w) = tokio::io::duplex(4096);

        let mut sender = LspTransport::new(client_r, client_w);
        let mut receiver = LspTransport::new(server_r, server_w);

        let resp = Message::Response(Response::ok(1i64.into(), json!({"capabilities": {}})));

        sender.send_message(&resp).await.unwrap();
        let received = receiver.recv_message().await.unwrap();

        match received {
            Message::Response(r) => {
                assert!(r.result.is_some());
                assert!(r.error.is_none());
            }
            other => panic!("expected Response, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn roundtrip_notification() {
        let (client_r, server_w) = tokio::io::duplex(4096);
        let (server_r, client_w) = tokio::io::duplex(4096);

        let mut sender = LspTransport::new(client_r, client_w);
        let mut receiver = LspTransport::new(server_r, server_w);

        let notif = Message::Notification(Notification::new(
            "textDocument/publishDiagnostics",
            Some(json!({"uri": "file:///test.v", "diagnostics": []})),
        ));

        sender.send_message(&notif).await.unwrap();
        let received = receiver.recv_message().await.unwrap();

        match received {
            Message::Notification(n) => {
                assert_eq!(n.method, "textDocument/publishDiagnostics");
            }
            other => panic!("expected Notification, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn multiple_messages() {
        let (client_r, server_w) = tokio::io::duplex(4096);
        let (server_r, client_w) = tokio::io::duplex(4096);

        let mut sender = LspTransport::new(client_r, client_w);
        let mut receiver = LspTransport::new(server_r, server_w);

        for i in 0..3 {
            let req = Message::Request(Request::new(i as i64, "test", None));
            sender.send_message(&req).await.unwrap();
        }

        for i in 0..3 {
            let received = receiver.recv_message().await.unwrap();
            match received {
                Message::Request(r) => {
                    assert_eq!(r.id, (i as i64).into());
                }
                other => panic!("expected Request, got {other:?}"),
            }
        }
    }
}

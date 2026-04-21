/// Test using LspTransport directly (no channels/tasks) to isolate the issue.
use rocqd::lsp::jsonrpc::{Message, Notification, Request};
use rocqd::lsp::transport::LspTransport;
use rocqd::lsp::vsrocq::{path_to_uri, UpdateHighlightsParams};
use serde_json::json;
use std::path::PathBuf;
use tokio::process::Command;

#[tokio::test]
async fn direct_transport_test() {
    if Command::new("vsrocqtop")
        .arg("--version")
        .output()
        .await
        .is_err()
    {
        eprintln!("skipping: vsrocqtop not found");
        return;
    }

    let fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/simple.v");
    let content = std::fs::read_to_string(&fixture_path).unwrap();
    let uri = path_to_uri(&fixture_path);

    let mut child = Command::new("vsrocqtop")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();

    let mut writer = LspTransport::new(tokio::io::empty(), stdin);
    let mut reader = LspTransport::new(stdout, tokio::io::sink());

    // Send initialize
    eprintln!(">>> initialize");
    writer
        .send_message(&Message::Request(Request::new(
            1i64,
            "initialize",
            Some(json!({
                "processId": std::process::id(),
                "capabilities": {},
                "rootUri": null,
                "initializationOptions": {
                    "proof": { "mode": 1, "delegation": "None", "block": true }
                }
            })),
        )))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Send initialized
    eprintln!(">>> initialized");
    writer
        .send_message(&Message::Notification(Notification::new(
            "initialized",
            Some(json!({})),
        )))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(1)).await;

    // Send didOpen
    eprintln!(">>> didOpen");
    writer
        .send_message(&Message::Notification(Notification::new(
            "textDocument/didOpen",
            Some(json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": "rocq",
                    "version": 1,
                    "text": content
                }
            })),
        )))
        .await
        .unwrap();

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // Send interpretToEnd
    eprintln!(">>> prover/interpretToEnd");
    writer
        .send_message(&Message::Notification(Notification::new(
            "prover/interpretToEnd",
            Some(json!({
                "textDocument": { "uri": uri, "version": 1 }
            })),
        )))
        .await
        .unwrap();

    // Read events
    eprintln!("<<< reading events...");
    let start = std::time::Instant::now();
    let mut count = 0;

    while start.elapsed() < std::time::Duration::from_secs(30) {
        match tokio::time::timeout(std::time::Duration::from_secs(10), reader.recv_message()).await
        {
            Ok(Ok(Message::Notification(n))) => {
                count += 1;
                if n.method == "prover/updateHighlights" {
                    if let Some(params) = &n.params {
                        let h: UpdateHighlightsParams =
                            serde_json::from_value(params.clone()).unwrap();
                        eprintln!(
                            "<<< [{count}] highlights: processed={}",
                            h.processed_range.len()
                        );
                        if !h.processed_range.is_empty() {
                            eprintln!("<<< DOCUMENT PROCESSED!");
                            child.kill().await.ok();
                            return;
                        }
                    }
                } else {
                    eprintln!("<<< [{count}] {}", n.method);
                }
            }
            Ok(Ok(Message::Response(r))) => {
                count += 1;
                eprintln!("<<< [{count}] response id={}", r.id);
            }
            Ok(Ok(Message::Request(r))) => {
                count += 1;
                eprintln!("<<< [{count}] request: {}", r.method);
            }
            Ok(Err(e)) => {
                eprintln!("<<< error: {e}");
                break;
            }
            Err(_) => {
                eprintln!("<<< timeout");
                break;
            }
        }
    }

    child.kill().await.ok();
    panic!("document not processed after {} events", count);
}

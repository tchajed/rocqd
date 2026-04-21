use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use lsp_types::{Diagnostic, DiagnosticSeverity, Position};
use sha2::{Digest, Sha256};
use tokio::time::{timeout, Duration};

use crate::lsp::vsrocq::{path_to_uri, VsRocqClient, VsRocqEvent};

/// Tracks the execution state of a file being checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionStatus {
    /// File has been opened but not yet fully processed.
    Processing,
    /// File has been fully processed (all ranges covered).
    Complete,
    /// Processing stopped due to an error.
    BlockedOnError { message: String },
}

/// A file session manages one .v file's interaction with vsrocqtop.
pub struct FileSession {
    /// Absolute path to the .v file.
    pub path: PathBuf,
    /// URI representation for LSP.
    uri: String,
    /// Last content sent to vsrocqtop.
    content: String,
    /// SHA-256 hash of the content.
    content_hash: String,
    /// Document version counter (increments on each didChange).
    version: i32,
    /// Accumulated diagnostics from the last compilation.
    pub diagnostics: Vec<Diagnostic>,
    /// Current execution status.
    pub status: ExecutionStatus,
    /// The vsrocqtop client.
    client: VsRocqClient,
}

impl FileSession {
    /// Open a new session for the given file path.
    pub async fn open(path: &Path) -> Result<Self> {
        let abs_path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()?.join(path)
        };
        let abs_path = abs_path
            .canonicalize()
            .with_context(|| format!("cannot resolve path: {}", path.display()))?;

        let content =
            tokio::fs::read_to_string(&abs_path)
                .await
                .with_context(|| format!("reading {}", abs_path.display()))?;
        let content_hash = hash_content(&content);
        let uri = path_to_uri(&abs_path);

        let mut client = VsRocqClient::spawn(None).await?;

        // Determine root URI from file's parent directory
        let root_uri = abs_path.parent().map(|p| path_to_uri(p));
        client
            .initialize(root_uri.as_deref())
            .await
            .context("LSP initialize")?;

        // Small delay to let vsrocqtop process init
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Open the document
        client.did_open(&uri, &content).await?;

        // Small delay to let vsrocqtop process didOpen
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Trigger full interpretation
        let version = 1;
        client.interpret_to_end(&uri, version).await?;

        Ok(Self {
            path: abs_path,
            uri,
            content,
            content_hash,
            version,
            diagnostics: Vec::new(),
            status: ExecutionStatus::Processing,
            client,
        })
    }

    /// Re-read the file and recompile if changed. Returns true if the file was
    /// re-sent.
    pub async fn recompile(&mut self) -> Result<bool> {
        let new_content = tokio::fs::read_to_string(&self.path)
            .await
            .with_context(|| format!("reading {}", self.path.display()))?;
        let new_hash = hash_content(&new_content);

        if new_hash == self.content_hash {
            tracing::info!("file unchanged, skipping recompile");
            return Ok(false);
        }

        self.version += 1;
        self.content = new_content;
        self.content_hash = new_hash;
        self.diagnostics.clear();
        self.status = ExecutionStatus::Processing;

        // Send full content change
        self.client
            .did_change(&self.uri, self.version, &self.content)
            .await?;

        // Small delay for vsrocqtop to process
        tokio::time::sleep(Duration::from_millis(100)).await;

        // Trigger re-interpretation
        self.client.interpret_to_end(&self.uri, self.version).await?;

        Ok(true)
    }

    /// Wait until vsrocqtop finishes processing (or errors out).
    /// Returns accumulated diagnostics.
    pub async fn wait_for_completion(&mut self, timeout_secs: u64) -> Result<&[Diagnostic]> {
        let deadline = Duration::from_secs(timeout_secs);

        let result = timeout(deadline, self.drain_events()).await;
        match result {
            Ok(Ok(())) => Ok(&self.diagnostics),
            Ok(Err(e)) => Err(e),
            Err(_) => bail!(
                "timed out waiting for vsrocqtop after {timeout_secs}s"
            ),
        }
    }

    /// Drain events from vsrocqtop until we detect completion.
    async fn drain_events(&mut self) -> Result<()> {
        let total_lines = self.content.lines().count() as u32;
        let last_line_len = self
            .content
            .lines()
            .last()
            .map(|l| l.len() as u32)
            .unwrap_or(0);

        loop {
            match self.client.recv_event().await? {
                Some(VsRocqEvent::Diagnostics { diagnostics, .. }) => {
                    self.diagnostics = diagnostics;
                    // If we have error diagnostics and processing has
                    // stalled (won't reach end of file), detect that.
                    if self.diagnostics.iter().any(|d| d.severity == Some(DiagnosticSeverity::ERROR)) {
                        self.status = ExecutionStatus::BlockedOnError {
                            message: self.diagnostics.iter()
                                .find(|d| d.severity == Some(DiagnosticSeverity::ERROR))
                                .map(|d| d.message.clone())
                                .unwrap_or_default(),
                        };
                        return Ok(());
                    }
                }
                Some(VsRocqEvent::UpdateHighlights(highlights)) => {
                    if covers_document(&highlights.processed_range, total_lines, last_line_len) {
                        self.status = ExecutionStatus::Complete;
                        return Ok(());
                    }
                }
                Some(VsRocqEvent::BlockOnError(block)) => {
                    self.status = ExecutionStatus::BlockedOnError {
                        message: block.message.clone(),
                    };
                    return Ok(());
                }
                None => {
                    // Should not happen since recv_event loops internally
                }
            }
        }
    }

    /// Run a query (Check, Print, etc.) against the compiled file.
    pub async fn query(
        &mut self,
        query_type: &str,
        line: u32,
        text: &str,
    ) -> Result<String> {
        let position = Position::new(line, 0);
        match query_type {
            "check" | "Check" => self.client.check(&self.uri, position, text).await,
            "print" | "Print" => self.client.print(&self.uri, position, text).await,
            "about" | "About" => self.client.about(&self.uri, position, text).await,
            other => bail!("unknown query type: {other}"),
        }
    }

    /// Check if there are any error diagnostics.
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|d| d.severity == Some(DiagnosticSeverity::ERROR))
    }

    /// Shut down the vsrocqtop process.
    pub async fn shutdown(mut self) -> Result<()> {
        self.client.shutdown().await
    }
}

/// Compute SHA-256 hash of content.
fn hash_content(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Check if a set of ranges covers the entire document.
/// Requires the processedRange to reach the last line AND cover at least
/// up to the last line's content length (to avoid false positives when
/// vsrocqtop stops at an error mid-line).
fn covers_document(ranges: &[lsp_types::Range], total_lines: u32, last_line_len: u32) -> bool {
    if total_lines == 0 {
        return true;
    }
    let last_line = total_lines.saturating_sub(1);
    ranges.iter().any(|r| {
        r.end.line > last_line || (r.end.line == last_line && r.end.character >= last_line_len)
    })
}

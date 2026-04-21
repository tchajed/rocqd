use std::path::PathBuf;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn vsrocqtop_available() -> bool {
    std::process::Command::new("vsrocqtop")
        .arg("--version")
        .output()
        .is_ok()
}

mod session_tests {
    use super::*;
    use rocqd::session::{ExecutionStatus, FileSession};

    #[tokio::test]
    async fn compile_simple_file() {
        if !vsrocqtop_available() {
            eprintln!("skipping: vsrocqtop not found");
            return;
        }

        let path = fixtures_dir().join("simple.v");
        let mut session = FileSession::open(&path).await.unwrap();
        let diagnostics = session.wait_for_completion(30).await.unwrap();

        assert!(
            diagnostics.is_empty(),
            "expected no diagnostics, got: {:?}",
            diagnostics
        );
        assert_eq!(session.status, ExecutionStatus::Complete);
        assert!(!session.has_errors());

        session.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn compile_error_file() {
        if !vsrocqtop_available() {
            eprintln!("skipping: vsrocqtop not found");
            return;
        }

        let path = fixtures_dir().join("error.v");
        let mut session = FileSession::open(&path).await.unwrap();
        let diagnostics = session.wait_for_completion(30).await.unwrap().to_vec();

        // Should detect error via diagnostics
        let has_problem = session.has_errors()
            || matches!(session.status, ExecutionStatus::BlockedOnError { .. });
        assert!(
            has_problem,
            "expected error, status={:?}, diagnostics={:?}",
            session.status, diagnostics
        );

        session.shutdown().await.unwrap();
    }
}

use std::path::PathBuf;
use whatsai_core::daemon::{MAX_SOCKET_PATH, socket_path};

#[test]
fn socket_path_length_is_validated_with_a_clear_message() {
    let short = PathBuf::from("/tmp/whatsai-state");
    assert_eq!(socket_path(&short).unwrap(), short.join("daemon.sock"));

    let longest_ok = PathBuf::from(format!(
        "/{}",
        "a".repeat(MAX_SOCKET_PATH - "/daemon.sock".len() - 1)
    ));
    assert_eq!(
        socket_path(&longest_ok).unwrap().as_os_str().len(),
        MAX_SOCKET_PATH
    );

    let too_long = PathBuf::from(format!("/{}", "a".repeat(MAX_SOCKET_PATH)));
    let error = socket_path(&too_long).unwrap_err().to_string();
    assert!(error.contains("too long for a Unix socket"), "{error}");
    assert!(error.contains("WHATSAI_STATE"), "{error}");
}

#[tokio::test]
async fn request_refuses_an_overlong_state_path_before_connecting() {
    let too_long = PathBuf::from(format!("/{}", "b".repeat(MAX_SOCKET_PATH)));
    let error = whatsai_core::daemon::request(&too_long, serde_json::json!({"action":"health"}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("too long for a Unix socket"), "{error}");
}

#[tokio::test]
async fn version_reports_the_release_and_schema() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mut c = whatsai_core::client::Client::open(tmp.path(), "V").unwrap();
    let v = c
        .command(serde_json::json!({"action":"version"}))
        .await
        .unwrap();
    assert_eq!(v["daemon"], env!("CARGO_PKG_VERSION"));
    assert_eq!(v["protocol"], whatsai_core::protocol::VERSION);
    assert!(v["database"].as_i64().unwrap() >= 5);
    let health = c
        .command(serde_json::json!({"action":"health"}))
        .await
        .unwrap();
    assert_eq!(health["release"], env!("CARGO_PKG_VERSION"));
}

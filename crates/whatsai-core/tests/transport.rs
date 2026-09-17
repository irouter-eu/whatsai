use serde_json::json;
use whatsai_core::{crypto::Identity, protocol::*, transport};
async fn roundtrip(relay: Option<&str>, forced: bool) -> anyhow::Result<String> {
    let a = transport::bind_mode(&Identity::generate("Alice"), relay, forced).await?;
    let b = transport::bind_mode(&Identity::generate("Bob"), relay, forced).await?;
    if forced {
        b.online().await;
        a.online().await;
    }
    let receiver = b.clone();
    let task = tokio::spawn(async move {
        let conn = receiver.accept().await.unwrap().await.unwrap();
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        let input = recv.read_to_end(MAX_FRAME).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&input).unwrap()["message"],
            "private peer message"
        );
        send.write_all(b"{\"ok\":true}").await.unwrap();
        send.finish().unwrap();
        conn.closed().await;
    });
    let (result, path) =
        transport::exchange(&a, b.addr(), &json!({"message":"private peer message"})).await?;
    assert_eq!(result["ok"], true);
    task.await?;
    a.close().await;
    b.close().await;
    Ok(path)
}
#[tokio::test]
async fn direct_quic_roundtrip() {
    assert_eq!(roundtrip(Some("off"), false).await.unwrap(), "direct");
}
#[tokio::test]
async fn forced_self_hosted_relay_roundtrip() {
    use iroh_relay::server::{RelayConfig, Server, ServerConfig};
    let mut config = ServerConfig::default();
    config.relay = Some(RelayConfig::new(
        "127.0.0.1:0".parse::<std::net::SocketAddr>().unwrap(),
    ));
    let relay = Server::spawn(config).await.unwrap();
    let url = format!("http://{}", relay.http_addr().unwrap());
    let path = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        roundtrip(Some(&url), true),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(path, "relay");
    relay.shutdown().await.unwrap();
}

#[test]
fn relay_configuration_defaults_to_public_relays() {
    use iroh::RelayMode;
    assert!(matches!(
        transport::relay_mode(None).unwrap(),
        RelayMode::Default
    ));
    assert!(matches!(
        transport::relay_mode(Some("default")).unwrap(),
        RelayMode::Default
    ));
    assert!(matches!(
        transport::relay_mode(Some(" off ")).unwrap(),
        RelayMode::Disabled
    ));
    assert!(matches!(
        transport::relay_mode(Some("https://relay.example.net")).unwrap(),
        RelayMode::Custom(_)
    ));
    assert!(transport::relay_mode(Some("not a url")).is_err());
}

#[tokio::test]
async fn a_remembered_port_is_reused_across_rebinds() {
    let identity = Identity::generate("Stable");
    let first = transport::bind_with(&identity, Some("off"), false, 0)
        .await
        .unwrap();
    let port = transport::bound_port(&first).unwrap();
    assert_ne!(port, 0);
    first.close().await;
    drop(first);
    // The kernel releases the socket once the endpoint is dropped; a restarted daemon is a new process.
    let mut second = None;
    for _ in 0..50 {
        match transport::bind_with(&identity, Some("off"), false, port).await {
            Ok(endpoint) => {
                second = Some(endpoint);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    let second = second.expect("port released after the previous endpoint was dropped");
    assert_eq!(transport::bound_port(&second), Some(port));
    assert!(second.addr().ip_addrs().any(|a| a.port() == port));
    second.close().await;
}

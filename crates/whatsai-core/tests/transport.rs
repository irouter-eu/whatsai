use serde_json::json;
use whatsai_core::{crypto::Identity, protocol::*, transport};
async fn roundtrip(relay: Option<&str>, forced: bool) -> anyhow::Result<String> {
    let a = transport::bind_mode(&Identity::generate("Alice"), relay, forced).await?;
    let b = transport::bind_mode(&Identity::generate("Bob"), relay, forced).await?;
    if relay.is_some() {
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
    assert_eq!(roundtrip(None, false).await.unwrap(), "direct");
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

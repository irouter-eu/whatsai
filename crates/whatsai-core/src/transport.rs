//! Private peer transport: explicit addresses, no public discovery or gossip.
use crate::{crypto::Identity, protocol::*};
use anyhow::{Result, ensure};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::Value;
use std::time::Duration;
pub async fn bind(identity: &Identity, relay: Option<&str>) -> Result<Endpoint> {
    bind_mode(identity, relay, false).await
}
pub async fn bind_mode(
    identity: &Identity,
    relay: Option<&str>,
    relay_only: bool,
) -> Result<Endpoint> {
    let secret: [u8; 32] = hex::decode(&identity.transport)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid transport key"))?;
    let mode = match relay {
        Some("default") => RelayMode::Default,
        Some(url) => RelayMode::custom([url.parse()?]),
        None => RelayMode::Disabled,
    };
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&secret))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(mode);
    if relay_only {
        ensure!(relay.is_some(), "relay-only mode requires a relay");
        builder = builder.clear_ip_transports();
    }
    Ok(builder.bind().await?)
}
pub async fn exchange(
    endpoint: &Endpoint,
    address: EndpointAddr,
    request: &Value,
) -> Result<(Value, String)> {
    tokio::time::timeout(Duration::from_secs(5), async {
        let connection = endpoint.connect(address, ALPN).await?;
        let (mut send, mut recv) = connection.open_bi().await?;
        let bytes = serde_json::to_vec(request)?;
        ensure!(bytes.len() <= MAX_FRAME, "peer request exceeds limit");
        send.write_all(&bytes).await?;
        send.finish()?;
        let bytes = recv.read_to_end(MAX_FRAME).await?;
        let response = serde_json::from_slice(&bytes)?;
        let path = connection
            .paths()
            .iter()
            .find(|p| p.is_selected())
            .map(|p| if p.is_relay() { "relay" } else { "direct" })
            .unwrap_or("unknown")
            .to_string();
        connection.close(0u8.into(), b"complete");
        Ok((response, path))
    })
    .await?
}

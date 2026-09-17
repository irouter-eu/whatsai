//! Private peer transport: explicit addresses, no public discovery or gossip.
use crate::{crypto::Identity, protocol::*};
use anyhow::{Result, ensure};
use iroh::{Endpoint, EndpointAddr, RelayMode, SecretKey, endpoint::presets};
use serde_json::Value;
use std::time::Duration;
/// Reaching an online peer, even through a relay, takes well under this; an offline one never answers.
pub const CONNECT_LIMIT: Duration = Duration::from_secs(6);
pub async fn bind(identity: &Identity, relay: Option<&str>) -> Result<Endpoint> {
    bind_mode(identity, relay, false).await
}
pub async fn bind_mode(
    identity: &Identity,
    relay: Option<&str>,
    relay_only: bool,
) -> Result<Endpoint> {
    bind_with(identity, relay, relay_only, 0).await
}
/// Bind on a fixed UDP `port` (0 for any) so a restarted daemon keeps the direct address its
/// teammates already know; without a relay that is the only way they can find it again.
pub async fn bind_with(
    identity: &Identity,
    relay: Option<&str>,
    relay_only: bool,
    port: u16,
) -> Result<Endpoint> {
    let secret: [u8; 32] = hex::decode(&identity.transport)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid transport key"))?;
    let mode = relay_mode(relay)?;
    if relay_only {
        ensure!(
            !matches!(mode, RelayMode::Disabled),
            "relay-only mode requires a relay"
        );
    }
    let mut builder = Endpoint::builder(presets::Minimal)
        .secret_key(SecretKey::from_bytes(&secret))
        .alpns(vec![ALPN.to_vec()])
        .relay_mode(mode);
    if relay_only {
        builder = builder.clear_ip_transports();
    } else if port != 0 {
        // Only the IPv4 socket is pinned: a dual-stack IPv6 socket on the same port would clash.
        builder = builder
            .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, port))?
            .bind_addr((std::net::Ipv6Addr::UNSPECIFIED, 0))?;
    }
    Ok(builder.bind().await?)
}
/// The IPv4 UDP port an endpoint bound, for persisting across restarts.
pub fn bound_port(endpoint: &Endpoint) -> Option<u16> {
    endpoint
        .bound_sockets()
        .iter()
        .find(|a| a.is_ipv4())
        .map(|a| a.port())
}
/// `WHATSAI_RELAY`: unset or `default` uses iroh's public relays so teams work across NATs with no
/// infrastructure; `off` allows direct connections only; a URL selects a self-hosted relay.
pub fn relay_mode(relay: Option<&str>) -> Result<RelayMode> {
    Ok(match relay.map(str::trim) {
        None | Some("") | Some("default") => RelayMode::Default,
        Some("off") | Some("none") | Some("disabled") => RelayMode::Disabled,
        Some(url) => RelayMode::custom([url.parse()?]),
    })
}
pub async fn exchange(
    endpoint: &Endpoint,
    address: EndpointAddr,
    request: &Value,
) -> Result<(Value, String)> {
    exchange_within(endpoint, address, request, Duration::from_secs(5)).await
}
pub async fn exchange_within(
    endpoint: &Endpoint,
    address: EndpointAddr,
    request: &Value,
    limit: Duration,
) -> Result<(Value, String)> {
    tokio::time::timeout(limit, async {
        let connection =
            tokio::time::timeout(limit.min(CONNECT_LIMIT), endpoint.connect(address, ALPN))
                .await
                .map_err(|_| anyhow::anyhow!("peer unreachable"))??;
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

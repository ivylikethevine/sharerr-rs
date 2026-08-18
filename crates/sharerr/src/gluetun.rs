//! Resolving the advertised endpoint from gluetun's control server.
//!
//! The deployment sharerr is built for — inside a gluetun network namespace, on a
//! provider-granted forwarded port — has neither a stable public IP nor a stable
//! inbound port. gluetun's control server (`:8000` by default) knows both:
//! `/v1/publicip/ip` reports the tunnel's exit address and
//! `/v1/openvpn/portforwarded` the forwarded port, for OpenVPN *and* WireGuard
//! despite the path's name.
//!
//! Two update paths feed the same [`AdvertisedEndpoint`]:
//!
//! * **The poll** ([`poll_loop`]) asks on a timer. It is the floor — it recovers
//!   a missed push and needs nothing configured on the gluetun side.
//! * **The push** is gluetun's `VPN_PORT_FORWARDING_UP_COMMAND` hitting
//!   `/gluetun/refresh` on this server, which only *nudges* the poller to ask
//!   now. The control server stays the single source of truth; nothing pushed is
//!   trusted directly, so the refresh endpoint needs no authentication beyond
//!   being reachable — it can only cause a question to be asked sooner.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sharerr_client::error_chain;
use url::Url;

use crate::state::ServeState;

/// What can go wrong asking gluetun.
#[derive(Debug, thiserror::Error)]
pub enum GluetunError {
    #[error("gluetun's control server at {url} could not be reached: {detail}")]
    Unreachable { url: String, detail: String },

    #[error("gluetun's control server answered {status} for {path}")]
    Status { status: u16, path: &'static str },

    #[error("gluetun sent a response this build could not read: {0}")]
    Malformed(String),

    /// The provider granted no forwarded port. Not a fault of this machine —
    /// gluetun implements forwarding for only some providers — but nothing can
    /// announce inbound without one, so the caller degrades to the static
    /// endpoint and says which mechanism is in use.
    #[error(
        "gluetun reports no forwarded port — the provider may not grant one; \
         falling back to the statically configured endpoint"
    )]
    NoForwardedPort,
}

type Result<T> = std::result::Result<T, GluetunError>;

/// A client for gluetun's control server.
#[derive(Debug, Clone)]
pub struct GluetunClient {
    http: reqwest::Client,
    base: Url,
}

#[derive(Debug, Deserialize)]
struct PublicIp {
    #[serde(default)]
    public_ip: String,
}

#[derive(Debug, Deserialize)]
struct ForwardedPort {
    #[serde(default)]
    port: u16,
}

impl GluetunClient {
    pub fn new(base: &Url) -> Result<Self> {
        let http = reqwest::Client::builder()
            // The control server is on loopback in the intended topology; a
            // longer wait means something is wrong, not slow.
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| GluetunError::Malformed(format!("building the HTTP client: {e}")))?;
        Ok(Self {
            http,
            base: sharerr_client::normalise_base(base),
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &'static str) -> Result<T> {
        let url = self
            .base
            .join(path)
            .map_err(|e| GluetunError::Malformed(format!("{} + {path}: {e}", self.base)))?;

        let response = self
            .http
            .get(url)
            .send()
            .await
            .map_err(|e| GluetunError::Unreachable {
                url: self.base.to_string(),
                detail: error_chain(&e),
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(GluetunError::Status {
                status: status.as_u16(),
                path,
            });
        }

        response
            .json()
            .await
            .map_err(|e| GluetunError::Malformed(error_chain(&e)))
    }

    /// The tunnel's exit address, per `/v1/publicip/ip`.
    pub async fn public_ip(&self) -> Result<IpAddr> {
        let body: PublicIp = self.get("v1/publicip/ip").await?;
        body.public_ip
            .parse()
            .map_err(|_| GluetunError::Malformed(format!("public_ip {:?}", body.public_ip)))
    }

    /// The forwarded port, per `/v1/openvpn/portforwarded`. `0` means the
    /// provider granted none.
    pub async fn forwarded_port(&self) -> Result<u16> {
        let body: ForwardedPort = self.get("v1/openvpn/portforwarded").await?;
        if body.port == 0 {
            return Err(GluetunError::NoForwardedPort);
        }
        Ok(body.port)
    }

    /// The base URL friends can currently reach this instance on: the exit
    /// address plus the forwarded port.
    pub async fn resolve_base(&self) -> Result<Url> {
        let (ip, port) = tokio::join!(self.public_ip(), self.forwarded_port());
        let (ip, port) = (ip?, port?);

        let raw = match ip {
            IpAddr::V4(v4) => format!("http://{v4}:{port}"),
            IpAddr::V6(v6) => format!("http://[{v6}]:{port}"),
        };
        Url::parse(&raw).map_err(|e| GluetunError::Malformed(format!("{raw}: {e}")))
    }
}

/// Keep the advertised endpoint in step with gluetun. Never returns.
///
/// Re-reads the configuration every iteration, so enabling gluetun (or changing
/// its address) through the settings page takes effect without a restart. When a
/// resolve *changes* the endpoint, the sync loop is woken — it refreshes every
/// stored torrent's announce URLs as part of its pass — rather than waiting for
/// the next scheduled run with every torrent announcing to a dead address.
pub async fn poll_loop(state: Arc<ServeState>) {
    // Logged once per transition, not per poll: a provider that grants no port
    // is a steady state, and one warning per minute forever is how a log stops
    // being read.
    let mut last_error: Option<String> = None;

    loop {
        let config = state.config().await;
        let interval = Duration::from_secs(config.gluetun.effective_poll_secs());

        if let Some(control) = &config.gluetun.control_url {
            match resolve_once(control).await {
                Ok(base) => {
                    if last_error.take().is_some() {
                        tracing::info!(%base, "gluetun endpoint resolution recovered");
                    }
                    if state.endpoint().observe(base.clone()) {
                        tracing::info!(
                            %base,
                            "advertised endpoint changed — waking the sync loop to \
                             re-announce and rewrite torrents"
                        );
                        state.request_sync();
                    }
                }
                Err(err) => {
                    let rendered = err.to_string();
                    if last_error.as_deref() != Some(&rendered) {
                        tracing::warn!(error = %rendered, "could not resolve the endpoint from gluetun");
                        last_error = Some(rendered);
                    }
                }
            }
        }

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = state.endpoint_refresh_requested() => {
                tracing::debug!("endpoint refresh requested — polling gluetun now");
            }
        }
    }
}

async fn resolve_once(control: &Url) -> Result<Url> {
    GluetunClient::new(control)?.resolve_base().await
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    async fn server_with(ip: serde_json::Value, port: serde_json::Value) -> MockServer {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ip))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/openvpn/portforwarded"))
            .respond_with(ResponseTemplate::new(200).set_body_json(port))
            .mount(&server)
            .await;
        server
    }

    fn client(server: &MockServer) -> GluetunClient {
        GluetunClient::new(&server.uri().parse().unwrap()).unwrap()
    }

    /// The faked gluetun control server the roadmap called for: the whole
    /// resolve path in tier 1, no VPN anywhere.
    #[tokio::test]
    async fn resolves_the_exit_address_and_forwarded_port_into_a_base_url() {
        let server = server_with(
            serde_json::json!({ "public_ip": "203.0.113.9", "country": "Elsewhere" }),
            serde_json::json!({ "port": 41234 }),
        )
        .await;

        assert_eq!(
            client(&server).resolve_base().await.unwrap().as_str(),
            "http://203.0.113.9:41234/"
        );
    }

    /// An IPv6 exit must come out bracketed, or the URL parse fails exactly the
    /// way the hand-typed field used to.
    #[tokio::test]
    async fn an_ipv6_exit_is_bracketed() {
        let server = server_with(
            serde_json::json!({ "public_ip": "2001:db8::9" }),
            serde_json::json!({ "port": 41234 }),
        )
        .await;

        assert_eq!(
            client(&server).resolve_base().await.unwrap().as_str(),
            "http://[2001:db8::9]:41234/"
        );
    }

    /// Port 0 is gluetun's way of saying the provider granted nothing. That must
    /// be its own error — the caller degrades to the static endpoint and says
    /// so — not a base URL with `:0` that every announce fails against.
    #[tokio::test]
    async fn a_zero_port_is_reported_as_no_forwarding() {
        let server = server_with(
            serde_json::json!({ "public_ip": "203.0.113.9" }),
            serde_json::json!({ "port": 0 }),
        )
        .await;

        assert!(matches!(
            client(&server).resolve_base().await.unwrap_err(),
            GluetunError::NoForwardedPort
        ));
    }

    #[tokio::test]
    async fn garbage_from_the_control_server_is_an_error_not_a_panic() {
        let server = server_with(
            serde_json::json!({ "public_ip": "not-an-address" }),
            serde_json::json!({ "port": 41234 }),
        )
        .await;

        assert!(matches!(
            client(&server).public_ip().await.unwrap_err(),
            GluetunError::Malformed(_)
        ));
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        // Port 9 (discard) on loopback: privileged, so nothing in a test run can
        // be listening there. A freed MockServer port is not usable here — a
        // parallel test's server can rebind it between drop and connect.
        let unreachable = GluetunClient::new(&"http://127.0.0.1:9".parse().unwrap()).unwrap();
        assert!(matches!(
            unreachable.public_ip().await.unwrap_err(),
            GluetunError::Unreachable { .. }
        ));
    }
}

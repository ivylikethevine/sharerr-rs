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
//!   `/gluetun/refresh`, and `VPN_PORT_FORWARDING_DOWN_COMMAND` hitting
//!   `/gluetun/down`, on this server. Both only *nudge* the poller — the up
//!   command asks it to resolve now instead of at the next tick, and the down
//!   command additionally forgets the dynamic history first
//!   ([`AdvertisedEndpoint::forget_dynamic`]), since the port it names is about
//!   to stop working and must not linger as a fallback for a resolve that can
//!   only refresh the exit address. The control server stays the single source
//!   of truth; nothing pushed is trusted directly, so neither endpoint needs
//!   authentication beyond being reachable — they can only cause a question to
//!   be asked sooner.
//!
//! The two lookups the poll makes are not equally reliable in practice: gluetun's
//! route-scoped auth config can grant a key access to `/v1/publicip/ip` and not
//! `/v1/openvpn/portforwarded` (or the reverse), and one can fail transiently
//! without the other. [`GluetunClient::resolve_base`] treats only the exit
//! address as load-bearing — a port lookup that fails falls back to the last
//! known port rather than blocking the whole resolve, so a VPN reconnect that
//! changes the exit address still gets advertised even when the port call is
//! stuck failing.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
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

    /// gluetun's control server has required an API key by default since
    /// v3.40; a request sent without one comes back `401` with a terse
    /// `Unauthorized` body, not a JSON error object.
    #[error(
        "gluetun's control server at {url} rejected the request as unauthorized — set \
         gluetun.api_key in Settings to the control server's API key \
         (CONTROL_SERVER_AUTH's apikey, from gluetun's config/auth file)"
    )]
    Unauthorized { url: String },
}

type Result<T> = std::result::Result<T, GluetunError>;

/// A client for gluetun's control server.
#[derive(Debug, Clone)]
pub struct GluetunClient {
    http: reqwest::Client,
    base: Url,
    api_key: Option<SecretString>,
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
    pub fn new(base: &Url, api_key: Option<SecretString>) -> Result<Self> {
        let http = reqwest::Client::builder()
            // The control server is on loopback in the intended topology; a
            // longer wait means something is wrong, not slow.
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| GluetunError::Malformed(format!("building the HTTP client: {e}")))?;
        Ok(Self {
            http,
            base: sharerr_client::normalise_base(base),
            api_key,
        })
    }

    async fn get<T: serde::de::DeserializeOwned>(&self, path: &'static str) -> Result<T> {
        let url = self
            .base
            .join(path)
            .map_err(|e| GluetunError::Malformed(format!("{} + {path}: {e}", self.base)))?;

        let mut request = self.http.get(url);
        if let Some(api_key) = &self.api_key {
            request = request.header("X-Api-Key", api_key.expose_secret());
        }

        let response = request
            .send()
            .await
            .map_err(|e| GluetunError::Unreachable {
                url: self.base.to_string(),
                detail: error_chain(&e),
            })?;

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GluetunError::Unauthorized {
                url: self.base.to_string(),
            });
        }
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
    ///
    /// `fallback_port`, when given, stands in for a forwarded-port lookup that
    /// fails — a mis-scoped API key (the roles in gluetun's own auth config can
    /// grant `/v1/publicip/ip` and not `/v1/openvpn/portforwarded`
    /// independently) or a transient error should not stop the exit address
    /// from being kept in step, or every reconnect would wait on the one call
    /// least likely to be reliable. Only a failed `public_ip` call is fatal —
    /// there is no address to build a base from at all without it.
    pub async fn resolve_base(&self, fallback_port: Option<u16>) -> Result<Url> {
        let (ip, port) = tokio::join!(self.public_ip(), self.forwarded_port());
        let ip = ip?;
        let port = match (port, fallback_port) {
            (Ok(port), _) => port,
            (Err(err), Some(fallback)) => {
                tracing::warn!(
                    error = %err,
                    fallback_port = fallback,
                    "could not refresh the forwarded port; keeping the last known one"
                );
                fallback
            }
            (Err(err), None) => return Err(err),
        };

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
            let api_key = gluetun_api_key(&state).await;

            // gluetun's control server has required an API key by default since
            // v3.40; sending an unkeyed request just to learn that again, every
            // interval, forever, on a deployment that has not been fixed yet is
            // a wasted round trip whose only possible answer is the same 401.
            // The config is re-read every iteration, so the moment a key is
            // saved through Settings, polling resumes on the next pass with no
            // restart needed.
            if let Some(api_key) = api_key {
                // A fallback for a forwarded-port lookup that fails on its own
                // — see `resolve_base` — read from what is currently
                // advertised rather than cached separately, so
                // `/gluetun/down` forgetting the dynamic history (a
                // known-dead port, not a flaky lookup) also clears this
                // fallback.
                let fallback_port = state.endpoint().current().and_then(|base| base.port());

                match resolve_once(control, Some(api_key), fallback_port).await {
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
            } else {
                let rendered = "no gluetun.api_key configured — skipping the poll rather \
                                 than sending a request that would only come back 401"
                    .to_owned();
                if last_error.as_deref() != Some(&rendered) {
                    tracing::warn!("{rendered}");
                    last_error = Some(rendered);
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

/// The control server's API key, if one is stored. A vault that will not open
/// (no master key, none of the settings ever saved one) means an unkeyed
/// request, same as before this key existed — the resulting `401` is what
/// [`GluetunError::Unauthorized`] exists to explain.
async fn gluetun_api_key(state: &ServeState) -> Option<SecretString> {
    let vault = state.open_vault().await.ok()?;
    vault
        .get(sharerr_core::config::secret_keys::GLUETUN_API_KEY)
        .ok()
        .flatten()
}

async fn resolve_once(
    control: &Url,
    api_key: Option<SecretString>,
    fallback_port: Option<u16>,
) -> Result<Url> {
    GluetunClient::new(control, api_key)?
        .resolve_base(fallback_port)
        .await
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
        GluetunClient::new(&server.uri().parse().unwrap(), None).unwrap()
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
            client(&server).resolve_base(None).await.unwrap().as_str(),
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
            client(&server).resolve_base(None).await.unwrap().as_str(),
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
            client(&server).resolve_base(None).await.unwrap_err(),
            GluetunError::NoForwardedPort
        ));
    }

    /// The resiliency the mis-scoped-API-key report called for: a port lookup
    /// that fails must not stop the exit address from being kept in step, as
    /// long as a fallback port is available.
    #[tokio::test]
    async fn a_failed_port_lookup_falls_back_to_the_last_known_port() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "public_ip": "203.0.113.9" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/openvpn/portforwarded"))
            .respond_with(ResponseTemplate::new(401).set_body_raw("Unauthorized\n", "text/plain"))
            .mount(&server)
            .await;

        assert_eq!(
            client(&server)
                .resolve_base(Some(41234))
                .await
                .unwrap()
                .as_str(),
            "http://203.0.113.9:41234/"
        );
    }

    /// Without a fallback, a failed port lookup is still fatal — there is
    /// nothing to build a base with otherwise.
    #[tokio::test]
    async fn a_failed_port_lookup_without_a_fallback_is_still_an_error() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "public_ip": "203.0.113.9" })),
            )
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/openvpn/portforwarded"))
            .respond_with(ResponseTemplate::new(401).set_body_raw("Unauthorized\n", "text/plain"))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server).resolve_base(None).await.unwrap_err(),
            GluetunError::Unauthorized { .. }
        ));
    }

    /// A failed exit-address lookup is fatal even with a fallback port — there
    /// is no address to build a base from at all.
    #[tokio::test]
    async fn a_failed_ip_lookup_is_fatal_even_with_a_fallback_port() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(ResponseTemplate::new(401).set_body_raw("Unauthorized\n", "text/plain"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/v1/openvpn/portforwarded"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({ "port": 41234 })))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server).resolve_base(Some(9999)).await.unwrap_err(),
            GluetunError::Unauthorized { .. }
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

    /// gluetun's own 401 body is `Unauthorized\n`, not JSON — this must be
    /// caught before the `.json()` parse, which would otherwise report it as
    /// [`GluetunError::Malformed`] and hide the real, actionable cause.
    #[tokio::test]
    async fn an_unauthorized_response_names_the_fix_instead_of_failing_to_parse() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(ResponseTemplate::new(401).set_body_raw("Unauthorized\n", "text/plain"))
            .mount(&server)
            .await;

        assert!(matches!(
            client(&server).public_ip().await.unwrap_err(),
            GluetunError::Unauthorized { .. }
        ));
    }

    /// When an API key is configured it must actually go out on the wire, or a
    /// stale gluetun deployment's `401` never clears no matter what is saved
    /// in Settings.
    #[tokio::test]
    async fn a_configured_api_key_is_sent_as_the_x_api_key_header() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .and(wiremock::matchers::header("X-Api-Key", "s3cret"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({ "public_ip": "203.0.113.9" })),
            )
            .mount(&server)
            .await;

        let keyed = GluetunClient::new(
            &server.uri().parse().unwrap(),
            Some(secrecy::SecretString::from("s3cret")),
        )
        .unwrap();
        assert_eq!(keyed.public_ip().await.unwrap().to_string(), "203.0.113.9");
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        // Port 9 (discard) on loopback: privileged, so nothing in a test run can
        // be listening there. A freed MockServer port is not usable here — a
        // parallel test's server can rebind it between drop and connect.
        let unreachable = GluetunClient::new(&"http://127.0.0.1:9".parse().unwrap(), None).unwrap();
        assert!(matches!(
            unreachable.public_ip().await.unwrap_err(),
            GluetunError::Unreachable { .. }
        ));
    }
}

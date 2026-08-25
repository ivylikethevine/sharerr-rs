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
//!
//! # Two independent pollers
//!
//! [`poll_loop`] is generic over [`GluetunTarget`] rather than assuming there is
//! one gluetun: `Tracker` keeps the announce/feed address in step, and `Client`
//! does the same for the torrent client's own tunnel when
//! it is a *separate* one — see `docker/deploy/dual-vpn/`. Both share this
//! module's client and error types;
//! they differ only in which `GluetunConfig`, `AdvertisedEndpoint`,
//! [`GluetunStatus`] and vault key they read and write, which
//! [`GluetunTarget`]'s methods resolve.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use sharerr_client::error_chain;
use sharerr_core::Config;
use sharerr_core::config::{GluetunConfig, secret_keys};
use sharerr_core::endpoint::{AdvertisedEndpoint, now_epoch};
use url::Url;

use crate::state::ServeState;

/// Which tunnel a gluetun poller is keeping in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GluetunTarget {
    /// The tracker/feed address — friends reach this instance here. The
    /// original, and still default, poller.
    Tracker,
    /// The torrent client's own address, when it sits behind a separate
    /// tunnel from the tracker's. Disabled by default; see
    /// `[gluetun_client]` in `sharerr.toml`.
    Client,
}

impl GluetunTarget {
    /// Parse the `?target=` query parameter `/gluetun/refresh` and
    /// `/gluetun/down` accept. Anything unrecognised, including absent, is
    /// `Tracker`, so callers that never send `target` keep working unchanged.
    pub fn from_query(raw: Option<&str>) -> Self {
        match raw {
            Some("client") => Self::Client,
            _ => Self::Tracker,
        }
    }

    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Tracker => "tracker",
            Self::Client => "client",
        }
    }

    pub(crate) fn config(self, config: &Config) -> &GluetunConfig {
        match self {
            Self::Tracker => &config.gluetun,
            Self::Client => &config.gluetun_client,
        }
    }

    pub(crate) fn api_key_secret(self) -> &'static str {
        match self {
            Self::Tracker => secret_keys::GLUETUN_API_KEY,
            Self::Client => secret_keys::GLUETUN_CLIENT_API_KEY,
        }
    }
}

/// What a gluetun poller last saw and last failed with, so the Diagnostics
/// page can show when gluetun last actually told sharerr something.
///
/// One lock around the whole snapshot rather than one per field — the same
/// shape as [`crate::lighthouse_client::LighthouseStatus`] — so a reader can
/// never see a poll's `last_poll_at` beside the previous poll's error.
#[derive(Debug, Default)]
pub struct GluetunStatus {
    inner: tokio::sync::RwLock<GluetunSnapshot>,
}

/// A read-only snapshot of a [`GluetunStatus`], for rendering.
#[derive(Debug, Clone, Default)]
pub struct GluetunSnapshot {
    pub last_poll_at: Option<i64>,
    pub last_success_at: Option<i64>,
    pub last_error: Option<String>,
}

impl GluetunStatus {
    async fn record_ok(&self) {
        let now = now_epoch();
        let mut inner = self.inner.write().await;
        inner.last_poll_at = Some(now);
        inner.last_success_at = Some(now);
        inner.last_error = None;
    }

    async fn record_err(&self, message: String) {
        let mut inner = self.inner.write().await;
        inner.last_poll_at = Some(now_epoch());
        inner.last_error = Some(message);
    }

    pub async fn snapshot(&self) -> GluetunSnapshot {
        self.inner.read().await.clone()
    }
}

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
    /// Build the HTTP client this type uses.
    ///
    /// Separate from [`Self::new`] so a caller that constructs a client
    /// repeatedly — the poller, once per interval, forever — can build one and
    /// keep it, rather than discarding a connection pool on every tick.
    pub fn http_client() -> Result<reqwest::Client> {
        // The control server is on loopback in the intended topology; a
        // longer wait means something is wrong, not slow.
        sharerr_client::http_client_with_timeout(Duration::from_secs(10))
            .map_err(|e| GluetunError::Malformed(e.to_string()))
    }

    pub fn new(base: &Url, api_key: Option<SecretString>) -> Result<Self> {
        Ok(Self::with_http(Self::http_client()?, base, api_key))
    }

    /// Reuse an existing HTTP client — see [`Self::http_client`].
    pub fn with_http(http: reqwest::Client, base: &Url, api_key: Option<SecretString>) -> Self {
        Self {
            http,
            base: sharerr_client::normalise_base(base),
            api_key,
        }
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
        if sharerr_client::is_auth_rejection(status) {
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

        // `SocketAddr`'s `Display` brackets an IPv6 literal the way a URL
        // authority needs it.
        let raw = format!("http://{}", std::net::SocketAddr::new(ip, port));
        Url::parse(&raw).map_err(|e| GluetunError::Malformed(format!("{raw}: {e}")))
    }
}

/// Keep `target`'s advertised endpoint in step with gluetun. Never returns.
///
/// Re-reads the configuration every iteration, so enabling gluetun (or changing
/// its address, or flipping `enabled`) through the settings page takes effect
/// without a restart. For [`GluetunTarget::Tracker`], a resolve that *changes*
/// the endpoint wakes the sync loop — it refreshes every stored torrent's
/// announce URLs as part of its pass — rather than waiting for the next
/// scheduled run with every torrent announcing to a dead address. A
/// [`GluetunTarget::Client`] change wakes nothing: nothing here rewrites a
/// torrent from it today, only gossip's self-record reads it, and that reads
/// live.
pub async fn poll_loop(state: Arc<ServeState>, target: GluetunTarget) {
    // Logged once per transition, not per poll: a provider that grants no port
    // is a steady state, and one warning per minute forever is how a log stops
    // being read.
    let mut last_error: Option<String> = None;
    let status = state.gluetun_status(target);
    let endpoint = state.endpoint_for(target);
    // Built once and reused for the life of the poller: `reqwest::Client` is an
    // Arc internally, so cloning it per poll shares its connection pool rather
    // than discarding one every interval. A build failure is kept rather than
    // returned, since this loop never returns; it is reported once per poll.
    let http = GluetunClient::http_client().map_err(|err| err.to_string());

    loop {
        let interval = poll_once(&state, target, &http, &status, &endpoint, &mut last_error).await;

        tokio::select! {
            () = tokio::time::sleep(interval) => {}
            () = state.endpoint_refresh_requested(target) => {
                tracing::debug!(target = target.label(), "endpoint refresh requested — polling gluetun now");
            }
        }
    }
}

/// One poll: resolve the endpoint if the poller is active, and record the
/// outcome. Split out of [`poll_loop`] so the resolve-and-record logic reads
/// apart from the loop/select scaffolding around it. Returns how long the
/// caller should wait before the next poll, read from the same config fetch
/// every path through this function needs anyway.
async fn poll_once(
    state: &ServeState,
    target: GluetunTarget,
    http: &std::result::Result<reqwest::Client, String>,
    status: &GluetunStatus,
    endpoint: &AdvertisedEndpoint,
    last_error: &mut Option<String>,
) -> Duration {
    let config = state.config().await;
    let gluetun_cfg = target.config(&config);
    let interval = Duration::from_secs(gluetun_cfg.effective_poll_secs());

    // An inactive poller is deliberately silent: being turned off, or having
    // no control server to ask, is a choice rather than a fault, and this
    // loop still runs (to notice it being re-enabled) so it must not add a
    // warning to the log every interval forever.
    let Some(control) = gluetun_cfg
        .control_url
        .as_ref()
        .filter(|_| gluetun_cfg.is_active())
    else {
        return interval;
    };

    // gluetun's control server has required an API key by default since
    // v3.40; sending an unkeyed request on every poll of an unfixed
    // deployment is a wasted round trip that can only 401. The config is
    // re-read every iteration, so saving a key through Settings resumes
    // polling on the next pass with no restart needed.
    let Some(api_key) = state.gluetun_api_key(target).await else {
        let rendered = format!(
            "no {} configured — skipping the poll rather than sending a \
             request that would only come back 401",
            target.api_key_secret()
        );
        if record_error(status, last_error, rendered.clone()).await {
            tracing::warn!(target = target.label(), "{rendered}");
        }
        return interval;
    };

    // A fallback for a forwarded-port lookup that fails on its own — see
    // `resolve_base` — read from the last *dynamic* observation rather than
    // cached separately, so `/gluetun/down` forgetting the dynamic history
    // (a known-dead port, not a flaky lookup) also clears this fallback.
    // Deliberately `last_observed`, not `current`: the latter falls back to
    // the static `advertised_host` port, and pairing the live VPN exit IP
    // with a port that was never forwarded there advertises an address
    // reachable nowhere — the first poll after start, or right after a
    // `/gluetun/down`, would otherwise rewrite every announce URL to it.
    let fallback_port = endpoint
        .last_observed()
        .map(|observed| observed.base)
        .and_then(|base| base.port());

    // The key comes from `ServeState::gluetun_api_key`, which caches it. A
    // vault that will not open (no master key, none of the settings ever saved
    // one) means no key at all, and the guard above skips the request rather
    // than sending one whose only possible answer is the `401` that
    // `GluetunError::Unauthorized` exists to explain.
    let resolved = match http {
        Ok(http) => {
            GluetunClient::with_http(http.clone(), control, Some(api_key))
                .resolve_base(fallback_port)
                .await
        }
        Err(err) => Err(GluetunError::Malformed(err.clone())),
    };

    match resolved {
        Ok(base) => {
            status.record_ok().await;
            if last_error.take().is_some() {
                tracing::info!(target = target.label(), %base, "gluetun endpoint resolution recovered");
            }
            if endpoint.observe(base.clone()) {
                tracing::info!(target = target.label(), %base, "advertised endpoint changed");
                if target == GluetunTarget::Tracker {
                    tracing::info!("waking the sync loop to re-announce and rewrite torrents");
                    state.request_sync();
                }
            }
        }
        Err(err) => {
            let rendered = err.to_string();
            if record_error(status, last_error, rendered.clone()).await {
                tracing::warn!(target = target.label(), error = %rendered, "could not resolve the endpoint from gluetun");
            }
        }
    }

    interval
}

/// Record `rendered` as this poll's failure, and report whether it is new
/// since the last one. The comparison, not just the recording, is what has
/// to happen identically at every failure site — logged once per transition
/// rather than once per poll, or a steady failure (no port granted, control
/// server down) turns into a warning every interval forever.
async fn record_error(
    status: &GluetunStatus,
    last_error: &mut Option<String>,
    rendered: String,
) -> bool {
    status.record_err(rendered.clone()).await;
    let changed = last_error.as_deref() != Some(&rendered);
    if changed {
        *last_error = Some(rendered);
    }
    changed
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)]

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
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({ "port": 41234 })),
            )
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

    /// A non-2xx, non-401 status (nothing 401-shaped catches this first) must
    /// surface as `Status`, not fall through to a JSON-decode `Malformed` that
    /// would hide the actual HTTP status from the operator.
    #[tokio::test]
    async fn a_non_401_failure_status_is_reported_as_status_not_malformed() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/v1/publicip/ip"))
            .respond_with(ResponseTemplate::new(404).set_body_raw("not found", "text/plain"))
            .mount(&server)
            .await;

        match client(&server).public_ip().await.unwrap_err() {
            GluetunError::Status { status, path } => {
                assert_eq!(status, 404);
                assert_eq!(path, "v1/publicip/ip");
            }
            other => panic!("expected Status, got {other:?}"),
        }
    }

    #[test]
    fn from_query_recognises_only_client_and_defaults_everything_else_to_tracker() {
        assert_eq!(
            GluetunTarget::from_query(Some("client")),
            GluetunTarget::Client
        );
        assert_eq!(
            GluetunTarget::from_query(Some("tracker")),
            GluetunTarget::Tracker
        );
        assert_eq!(
            GluetunTarget::from_query(Some("bogus")),
            GluetunTarget::Tracker
        );
        assert_eq!(GluetunTarget::from_query(None), GluetunTarget::Tracker);
    }

    /// Tracker and Client must resolve to genuinely different config sections
    /// and vault keys — a poller that accidentally shared either would keep two
    /// independent tunnels in lockstep instead of apart.
    #[test]
    fn tracker_and_client_targets_resolve_to_distinct_config_and_secret_keys() {
        let mut config = Config::default();
        config.gluetun.poll_secs = 11;
        config.gluetun_client.poll_secs = 22;

        assert_eq!(GluetunTarget::Tracker.label(), "tracker");
        assert_eq!(GluetunTarget::Client.label(), "client");
        assert_eq!(GluetunTarget::Tracker.config(&config).poll_secs, 11);
        assert_eq!(GluetunTarget::Client.config(&config).poll_secs, 22);
        assert_ne!(
            GluetunTarget::Tracker.api_key_secret(),
            GluetunTarget::Client.api_key_secret()
        );
    }

    #[tokio::test]
    async fn status_snapshot_reflects_record_ok_and_record_err() {
        let status = GluetunStatus::default();
        let empty = status.snapshot().await;
        assert!(empty.last_poll_at.is_none());
        assert!(empty.last_success_at.is_none());
        assert!(empty.last_error.is_none());

        status.record_ok().await;
        let ok = status.snapshot().await;
        assert!(ok.last_poll_at.is_some());
        assert!(ok.last_success_at.is_some());
        assert!(ok.last_error.is_none());

        status.record_err("could not reach it".to_owned()).await;
        let failed = status.snapshot().await;
        assert!(failed.last_poll_at.is_some());
        // A later error must not erase the last time a poll actually succeeded —
        // that is what tells an operator "it is failing now" apart from
        // "it never worked".
        assert_eq!(failed.last_success_at, ok.last_success_at);
        assert_eq!(failed.last_error.as_deref(), Some("could not reach it"));
    }

    /// The de-dup that keeps a steady failure (no port granted, control server
    /// down) from becoming a warning on every single poll forever.
    #[tokio::test]
    async fn record_error_only_reports_the_first_occurrence_of_a_message() {
        let status = GluetunStatus::default();
        let mut last_error = None;

        assert!(record_error(&status, &mut last_error, "boom".to_owned()).await);
        assert!(!record_error(&status, &mut last_error, "boom".to_owned()).await);
        assert!(record_error(&status, &mut last_error, "different boom".to_owned()).await);
    }

    /// An inactive poller — the default, with no `control_url` configured —
    /// must not touch the status at all: being off is a choice, not a fault.
    #[tokio::test]
    async fn poll_once_is_silent_when_the_poller_is_inactive() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        let target = GluetunTarget::Tracker;
        let status = state.gluetun_status(target);
        let endpoint = state.endpoint_for(target);
        let http = GluetunClient::http_client().map_err(|e| e.to_string());
        let mut last_error = None;

        let interval = poll_once(&state, target, &http, &status, &endpoint, &mut last_error).await;

        assert_eq!(interval, Duration::from_secs(60), "default poll_secs");
        assert!(status.snapshot().await.last_poll_at.is_none());
    }

    /// Active, but no API key available (no vault configured in this fixture) —
    /// must record the miss rather than sending a request that could only 401.
    #[tokio::test]
    async fn poll_once_records_a_missing_api_key_without_polling_gluetun() {
        let (_dir, state) = crate::state::fixtures::unconfigured();
        let target = GluetunTarget::Tracker;

        let mut config = state.config().await;
        config.gluetun.control_url = Some("http://127.0.0.1:9".parse().unwrap());
        state.replace_config(config).await;

        let status = state.gluetun_status(target);
        let endpoint = state.endpoint_for(target);
        let http = GluetunClient::http_client().map_err(|e| e.to_string());
        let mut last_error = None;

        poll_once(&state, target, &http, &status, &endpoint, &mut last_error).await;

        let snapshot = status.snapshot().await;
        assert!(snapshot.last_poll_at.is_some());
        assert!(
            snapshot
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains(target.api_key_secret())),
            "{:?}",
            snapshot.last_error
        );
        assert!(
            last_error.is_some(),
            "the loop's own dedup state must be updated too"
        );
    }

    /// `poll_once`'s two early returns (inactive, no API key) are covered above;
    /// this — and the failure test beside it — are what's left: the actual
    /// resolve attempt once both guards are past. That needs a real vault
    /// holding the target's API key, which means `SHARERR_MASTER_KEY` in the
    /// process env — see `gossip.rs`'s tests for why that requires a `Jail`
    /// (clears/serializes the env) and a `#[test]` driving its own runtime
    /// rather than `#[tokio::test]`, which would already hold one on this
    /// thread.
    #[test]
    fn poll_once_resolves_successfully_and_advertises_the_new_endpoint() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let server = server_with(
                    serde_json::json!({ "public_ip": "203.0.113.9" }),
                    serde_json::json!({ "port": 41234 }),
                )
                .await;

                let config = Config {
                    data_dir: jail.directory().to_path_buf(),
                    gluetun: GluetunConfig {
                        control_url: Some(server.uri().parse().unwrap()),
                        ..GluetunConfig::default()
                    },
                    ..Config::default()
                };
                let target = GluetunTarget::Tracker;

                let mut vault = sharerr_store::Vault::open(
                    config.vault_path(),
                    &SecretString::from("a-master-key"),
                )
                .unwrap();
                vault
                    .put(target.api_key_secret(), &SecretString::from("a-key"))
                    .unwrap();
                drop(vault);

                let state = crate::state::ServeState::new(
                    config,
                    jail.directory().join("sharerr.toml"),
                    None,
                );
                let status = state.gluetun_status(target);
                let endpoint = state.endpoint_for(target);
                let http = GluetunClient::http_client().map_err(|e| e.to_string());
                let mut last_error = None;

                poll_once(&state, target, &http, &status, &endpoint, &mut last_error).await;

                let snapshot = status.snapshot().await;
                assert!(snapshot.last_poll_at.is_some());
                assert!(snapshot.last_error.is_none(), "{:?}", snapshot.last_error);
                assert_eq!(
                    endpoint.current().as_ref().map(Url::as_str),
                    Some("http://203.0.113.9:41234/"),
                    "a successful resolve must advertise the new endpoint"
                );
            });
            Ok(())
        });
    }

    #[test]
    fn poll_once_records_a_resolve_failure_when_the_control_server_is_unreachable() {
        figment::Jail::expect_with(|jail| {
            jail.clear_env();
            jail.set_env("SHARERR_MASTER_KEY", "a-master-key");
            let runtime = tokio::runtime::Runtime::new().unwrap();
            runtime.block_on(async {
                let config = Config {
                    data_dir: jail.directory().to_path_buf(),
                    gluetun: GluetunConfig {
                        // Nothing listens on port 9 ("discard") — a stand-in for
                        // gluetun being unreachable, same trick the missing-key
                        // test above uses.
                        control_url: Some("http://127.0.0.1:9".parse().unwrap()),
                        ..GluetunConfig::default()
                    },
                    ..Config::default()
                };
                let target = GluetunTarget::Tracker;

                let mut vault = sharerr_store::Vault::open(
                    config.vault_path(),
                    &SecretString::from("a-master-key"),
                )
                .unwrap();
                vault
                    .put(target.api_key_secret(), &SecretString::from("a-key"))
                    .unwrap();
                drop(vault);

                let state = crate::state::ServeState::new(
                    config,
                    jail.directory().join("sharerr.toml"),
                    None,
                );
                let status = state.gluetun_status(target);
                let endpoint = state.endpoint_for(target);
                let http = GluetunClient::http_client().map_err(|e| e.to_string());
                let mut last_error = None;

                poll_once(&state, target, &http, &status, &endpoint, &mut last_error).await;

                let snapshot = status.snapshot().await;
                assert!(snapshot.last_poll_at.is_some());
                assert!(snapshot.last_error.is_some());
                assert!(last_error.is_some());
                assert!(endpoint.current().is_none());
            });
            Ok(())
        });
    }
}

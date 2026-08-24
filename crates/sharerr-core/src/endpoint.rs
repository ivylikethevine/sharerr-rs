//! The one resolver for this instance's externally reachable address.
//!
//! `Config::public_base_url()` and the tracker provider both need "the URL a
//! friend reaches this instance on" built from the same two config fields — one
//! route, [`advertised_base`], keeps them from drifting apart the moment the
//! endpoint starts changing at runtime. Anything that appends a path to it goes
//! through [`join_path`].
//!
//! `tracker.advertised_url` carries scheme, path prefix, and IPv6 brackets —
//! what a reverse-proxied or IPv6 self-hosted setup needs and a bare host:port
//! cannot express. `advertised_host` stays for the plain case, with IPv6 literals
//! bracketed automatically.

use std::net::{IpAddr, Ipv6Addr};
use std::sync::RwLock;
use std::time::{SystemTime, UNIX_EPOCH};

use url::Url;

/// Whether `ip` is loopback, link-local, or otherwise private — an address no
/// friend outside this network could ever reach.
///
/// Used both to refuse the gluetun refresh nudge from anywhere but a private
/// neighbour, and to catch a hand-typed `tracker.advertised_host` that could
/// never work for anyone outside the operator's own network.
pub fn is_private_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(v6) => v6.is_loopback() || (v6.segments()[0] & 0xfe00) == 0xfc00,
    }
}

use crate::config::TrackerConfig;

/// What stops an advertised address becoming a URL.
#[derive(Debug, thiserror::Error)]
pub enum EndpointError {
    #[error("could not build a URL from advertised host {host:?} port {port}: {source}")]
    BadHost {
        host: String,
        port: u16,
        #[source]
        source: url::ParseError,
    },
}

/// The configured base URL friends reach this instance on, or `None` when the
/// operator has not said.
///
/// `tracker.advertised_url` wins when set — it is the expressive form, carrying
/// scheme, port, and path prefix in one value. Otherwise `advertised_host` (plus
/// `tracker.port`, falling back to `server_port`) builds the plain
/// `http://host:port` form. An IPv6 literal host is bracketed automatically.
pub fn advertised_base(
    tracker: &TrackerConfig,
    server_port: u16,
) -> Result<Option<Url>, EndpointError> {
    if let Some(url) = &tracker.advertised_url {
        return Ok(Some(url.clone()));
    }

    let Some(host) = tracker.advertised_host.as_deref() else {
        return Ok(None);
    };
    let port = tracker.port.unwrap_or(server_port);
    let host = bracket_ipv6(host);

    Url::parse(&format!("http://{host}:{port}"))
        .map(Some)
        .map_err(|source| EndpointError::BadHost {
            host: host.into_owned(),
            port,
            source,
        })
}

/// A base URL rendered without its trailing slash, so `format!("{base}{path}")`
/// composes correctly. `Url` prints a bare origin as `http://host:8477/`, and
/// that slash would otherwise double up against every path constant.
pub fn base_string(base: &Url) -> String {
    let mut rendered = base.to_string();
    while rendered.ends_with('/') {
        rendered.pop();
    }
    rendered
}

/// `base` with `path` appended, keeping any path prefix the base carries.
///
/// Deliberately not `Url::join`: joining an absolute path *replaces* the base's
/// path, which silently strips the `/sharerr` prefix a reverse-proxied setup
/// depends on. `path` must start with `/`.
pub fn join_path(base: &Url, path: &str) -> String {
    debug_assert!(path.starts_with('/'), "join_path takes an absolute path");
    format!("{}{path}", base_string(base))
}

/// Wrap a bare IPv6 literal in the brackets a URL authority requires.
fn bracket_ipv6(host: &str) -> std::borrow::Cow<'_, str> {
    if !host.starts_with('[') && host.parse::<Ipv6Addr>().is_ok() {
        std::borrow::Cow::Owned(format!("[{host}]"))
    } else {
        std::borrow::Cow::Borrowed(host)
    }
}

/// How many dynamically observed endpoints are remembered.
///
/// Enough to span a few VPN reconnects — each one changes the forwarded port, and
/// a torrent already sitting in a friend's client keeps whatever announce list it
/// downloaded with. More would pad every `.torrent` with tiers that are almost
/// certainly dead.
const MAX_DYNAMIC_HISTORY: usize = 4;

/// One dynamically observed base, with when it was last seen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedBase {
    pub base: Url,
    /// Unix seconds of the most recent observation.
    pub observed_at: i64,
}

/// The externally reachable endpoint, as a live value rather than a config field.
///
/// The deployment sharerr is built for — behind gluetun, on a provider-granted
/// forwarded port — has neither a stable public IP nor a stable inbound port, so
/// "the address friends reach this instance on" is *resolved*, not typed. This
/// type is the one place that answer lives: the static base from configuration,
/// plus whatever the gluetun poller (or its push nudge) has observed, most recent
/// first.
///
/// A short history is kept on purpose. A reconnect that briefly returns an old
/// exit is remembered rather than trusted alone, and the history becomes the
/// announce *list* in every torrent built — so a rotated port leaves a friend's
/// client with older tiers to fall back through instead of one dead address.
///
/// `std::sync::RwLock`, not tokio's: every operation is a few pointer moves, and
/// no guard is ever held across an `.await`.
#[derive(Debug)]
pub struct AdvertisedEndpoint {
    inner: RwLock<Inner>,
}

#[derive(Debug)]
struct Inner {
    static_base: Option<Url>,
    /// Most recent first.
    dynamic: Vec<ObservedBase>,
}

impl AdvertisedEndpoint {
    pub fn new(static_base: Option<Url>) -> Self {
        Self {
            inner: RwLock::new(Inner {
                static_base,
                dynamic: Vec::new(),
            }),
        }
    }

    /// Adopt a rewritten configuration without losing the dynamic history.
    ///
    /// The settings page can change `advertised_host` while the poller has a
    /// perfectly good observed endpoint; replacing this whole value would forget
    /// it and briefly advertise the stale static address.
    pub fn set_static(&self, static_base: Option<Url>) {
        if let Ok(mut inner) = self.inner.write() {
            inner.static_base = static_base;
        }
    }

    /// The base URL to advertise right now: the most recent observation, or the
    /// configured static base when nothing has been observed.
    ///
    /// Observation wins over configuration — the poller asked gluetun seconds
    /// ago; the config field was typed once. An operator who wants the static
    /// value to win simply does not configure the poller.
    pub fn current(&self) -> Option<Url> {
        let inner = self.inner.read().ok()?;
        inner
            .dynamic
            .first()
            .map(|o| o.base.clone())
            .or_else(|| inner.static_base.clone())
    }

    /// The most recent *dynamic* observation, with when it was seen — `None`
    /// when nothing has ever been observed, even if a static base is
    /// configured. Unlike [`Self::current`], this never falls back to the
    /// static base: it answers "what did gluetun last actually report",
    /// which is a different question from "what would sharerr advertise right
    /// now" whenever the two differ.
    pub fn last_observed(&self) -> Option<ObservedBase> {
        let inner = self.inner.read().ok()?;
        inner.dynamic.first().cloned()
    }

    /// Record an observed base. Returns `true` when this *changes* the current
    /// endpoint — the signal to rewrite announce lists and wake the sync loop.
    ///
    /// Re-observing the current endpoint refreshes its timestamp and returns
    /// `false`; re-observing an *older* one promotes it back to the front, the
    /// way a reconnect that lands on a previous exit really does move the
    /// reachable address back there.
    pub fn observe(&self, base: Url) -> bool {
        let Ok(mut inner) = self.inner.write() else {
            return false;
        };
        let now = now_epoch();

        if let Some(front) = inner.dynamic.first_mut()
            && front.base == base
        {
            front.observed_at = now;
            return false;
        }

        inner.dynamic.retain(|o| o.base != base);
        inner.dynamic.insert(
            0,
            ObservedBase {
                base,
                observed_at: now,
            },
        );
        inner.dynamic.truncate(MAX_DYNAMIC_HISTORY);
        true
    }

    /// Forget every dynamically observed base, falling back to the static one
    /// (if any) until the next successful resolve.
    ///
    /// Used when gluetun says the forwarded port has gone away
    /// (`VPN_PORT_FORWARDING_DOWN_COMMAND`): the most recent observation is
    /// known-dead, not merely stale, so it must not survive as a fallback port
    /// for the next resolve that can only refresh the exit address.
    pub fn forget_dynamic(&self) {
        if let Ok(mut inner) = self.inner.write() {
            inner.dynamic.clear();
        }
    }

    /// Every base worth announcing on, most current first: the dynamic history,
    /// then the static base if it is not already among them. This is the announce
    /// list a newly built torrent carries.
    pub fn recent(&self) -> Vec<Url> {
        let Ok(inner) = self.inner.read() else {
            return Vec::new();
        };
        let mut bases: Vec<Url> = inner.dynamic.iter().map(|o| o.base.clone()).collect();
        if let Some(static_base) = &inner.static_base
            && !bases.contains(static_base)
        {
            bases.push(static_base.clone());
        }
        bases
    }
}

/// How far ahead of this host's clock a peer-supplied timestamp
/// (`EndpointRecord::signed_at`, a gossiped `observed_at`) may be and still
/// be trusted.
///
/// A stale timestamp erodes on its own as real time passes it; a
/// too-future one does not — left unchecked it locks out every genuine
/// update behind it until this host's clock catches up to it, which for a
/// clock set wildly wrong is never. Used at every point that ingests a
/// peer's own clock: `sharerr::gossip::ingest` and
/// `Store::record_peer_endpoint`; the lighthouse (a separate crate, no
/// friend relationship to reuse this constant through) enforces the same
/// five minutes independently.
pub const MAX_FUTURE_SKEW_SECS: i64 = 5 * 60;

/// Current Unix time in seconds, saturating to `0` if the clock is somehow
/// before the epoch. The one place this is computed; reused wherever a
/// timestamp is stamped onto a row or a record.
pub fn now_epoch() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn tracker(host: Option<&str>, port: Option<u16>, url: Option<&str>) -> TrackerConfig {
        TrackerConfig {
            backend: (),
            advertised_host: host.map(str::to_owned),
            port,
            advertised_url: url.map(|u| Url::parse(u).unwrap()),
            bind: None,
        }
    }

    #[test]
    fn nothing_configured_resolves_to_nothing() {
        assert!(
            advertised_base(&tracker(None, None, None), 8477)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn a_plain_host_builds_the_classic_url() {
        let base = advertised_base(&tracker(Some("sharerr.example"), None, None), 8477)
            .unwrap()
            .unwrap();
        assert_eq!(base_string(&base), "http://sharerr.example:8477");

        let with_port = advertised_base(&tracker(Some("sharerr.example"), Some(9000), None), 8477)
            .unwrap()
            .unwrap();
        assert_eq!(base_string(&with_port), "http://sharerr.example:9000");
    }

    /// An unbracketed IPv6 host would produce `http://2001:db8::1:8477`, which
    /// no URL parser accepts.
    #[test]
    fn an_ipv6_literal_gains_brackets() {
        let base = advertised_base(&tracker(Some("2001:db8::1"), None, None), 8477)
            .unwrap()
            .unwrap();
        assert_eq!(base_string(&base), "http://[2001:db8::1]:8477");

        // Already bracketed stays as it is.
        let bracketed = advertised_base(&tracker(Some("[2001:db8::1]"), None, None), 8477)
            .unwrap()
            .unwrap();
        assert_eq!(base_string(&bracketed), "http://[2001:db8::1]:8477");
    }

    /// The expressive form: scheme, non-default path prefix, and its own port —
    /// exactly what a reverse-proxied deployment needs and the host field cannot
    /// say.
    #[test]
    fn an_advertised_url_wins_and_keeps_its_prefix() {
        let config = tracker(
            // The host is deliberately also set: the URL must win, or two fields
            // would fight and the loser would be silently ignored.
            Some("ignored.example"),
            Some(1234),
            Some("https://proxy.example/sharerr"),
        );
        let base = advertised_base(&config, 8477).unwrap().unwrap();

        assert_eq!(base_string(&base), "https://proxy.example/sharerr");
        assert_eq!(
            join_path(&base, "/announce"),
            "https://proxy.example/sharerr/announce",
            "Url::join would have replaced the prefix; this must append"
        );
    }

    #[test]
    fn join_path_survives_a_bare_origin() {
        let base = Url::parse("http://sharerr.example:8477").unwrap();
        assert_eq!(
            join_path(&base, "/announce"),
            "http://sharerr.example:8477/announce"
        );
    }

    // ---------------------------------------------------- the live endpoint

    fn url(raw: &str) -> Url {
        Url::parse(raw).unwrap()
    }

    #[test]
    fn with_no_observations_the_static_base_is_current() {
        let endpoint = AdvertisedEndpoint::new(Some(url("http://static.example:8477")));
        assert_eq!(endpoint.current(), Some(url("http://static.example:8477")));
        assert_eq!(endpoint.recent(), vec![url("http://static.example:8477")]);

        let unset = AdvertisedEndpoint::new(None);
        assert_eq!(unset.current(), None);
        assert!(unset.recent().is_empty());
    }

    /// The core of the gluetun story: an observation beats the config field, and
    /// the change is reported so the caller can react rather than waiting for the
    /// next natural pass.
    #[test]
    fn an_observation_wins_over_the_static_base_and_reports_the_change() {
        let endpoint = AdvertisedEndpoint::new(Some(url("http://static.example:8477")));

        assert!(endpoint.observe(url("http://203.0.113.9:41234")));
        assert_eq!(endpoint.current(), Some(url("http://203.0.113.9:41234")));

        // Re-observing the same endpoint is the steady state, not a change.
        assert!(!endpoint.observe(url("http://203.0.113.9:41234")));
    }

    /// The announce list a torrent gets: recent endpoints first, the static base
    /// last, nothing repeated.
    #[test]
    fn recent_spans_the_history_and_ends_with_the_static_base() {
        let endpoint = AdvertisedEndpoint::new(Some(url("http://static.example:8477")));
        endpoint.observe(url("http://203.0.113.9:41234"));
        endpoint.observe(url("http://203.0.113.9:52345"));

        assert_eq!(
            endpoint.recent(),
            vec![
                url("http://203.0.113.9:52345"),
                url("http://203.0.113.9:41234"),
                url("http://static.example:8477"),
            ]
        );
    }

    /// A reconnect that lands back on a previously held exit must promote it,
    /// not duplicate it.
    #[test]
    fn reobserving_an_old_endpoint_promotes_it_without_duplication() {
        let endpoint = AdvertisedEndpoint::new(None);
        endpoint.observe(url("http://203.0.113.1:1000"));
        endpoint.observe(url("http://203.0.113.2:2000"));

        assert!(endpoint.observe(url("http://203.0.113.1:1000")));
        assert_eq!(
            endpoint.recent(),
            vec![
                url("http://203.0.113.1:1000"),
                url("http://203.0.113.2:2000")
            ]
        );
    }

    #[test]
    fn the_history_is_bounded() {
        let endpoint = AdvertisedEndpoint::new(None);
        for port in 1..=10u16 {
            endpoint.observe(url(&format!("http://203.0.113.9:{port}")));
        }
        assert_eq!(endpoint.recent().len(), MAX_DYNAMIC_HISTORY);
        assert_eq!(endpoint.current(), Some(url("http://203.0.113.9:10")));
    }

    /// The down-command path: a dead observation must not linger as `current()`
    /// or as a fallback for the next resolve, but the static base still stands.
    #[test]
    fn forgetting_dynamic_state_falls_back_to_the_static_base() {
        let endpoint = AdvertisedEndpoint::new(Some(url("http://static.example:8477")));
        endpoint.observe(url("http://203.0.113.9:41234"));

        endpoint.forget_dynamic();

        assert_eq!(endpoint.current(), Some(url("http://static.example:8477")));
        assert_eq!(endpoint.recent(), vec![url("http://static.example:8477")]);
    }

    /// A settings save must not wipe what the poller knows.
    #[test]
    fn replacing_the_static_base_keeps_the_dynamic_history() {
        let endpoint = AdvertisedEndpoint::new(Some(url("http://old.example:8477")));
        endpoint.observe(url("http://203.0.113.9:41234"));

        endpoint.set_static(Some(url("http://new.example:8477")));

        assert_eq!(endpoint.current(), Some(url("http://203.0.113.9:41234")));
        assert_eq!(
            endpoint.recent(),
            vec![
                url("http://203.0.113.9:41234"),
                url("http://new.example:8477"),
            ]
        );
    }
}

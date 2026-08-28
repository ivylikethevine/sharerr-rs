//! `TorrentClient` for [`RtorrentClient`] — turning rTorrent's untyped
//! XML-RPC scalars into sharerr's typed [`TorrentSummary`]/[`TorrentFileEntry`].

use async_trait::async_trait;
use sharerr_client::{
    AddRequest, ClientKind, Result, TorrentClient, TorrentFileEntry, TorrentSummary,
};
use url::Url;

use crate::KIND;
use crate::client::RtorrentClient;
use crate::xmlrpc::{Param, XmlValue, quote_command_arg, take};

/// The view `d.multicall2` iterates. rTorrent's built-in "everything loaded"
/// view, present on every install without configuration.
pub(crate) const MAIN_VIEW: &str = "main";

fn as_str(value: &XmlValue) -> &str {
    match value {
        XmlValue::Str(s) => s,
        XmlValue::Int(_) | XmlValue::Array(_) | XmlValue::Struct(_) => "",
    }
}

fn as_bool(value: &XmlValue) -> bool {
    match value {
        XmlValue::Int(n) => *n != 0,
        XmlValue::Str(s) => s != "0" && !s.is_empty(),
        XmlValue::Array(_) | XmlValue::Struct(_) => false,
    }
}

fn as_u64(value: &XmlValue) -> u64 {
    match value {
        XmlValue::Int(n) => (*n).try_into().unwrap_or(0),
        XmlValue::Str(s) => s.trim().parse().unwrap_or(0),
        XmlValue::Array(_) | XmlValue::Struct(_) => 0,
    }
}

#[async_trait]
impl TorrentClient for RtorrentClient {
    fn kind(&self) -> ClientKind {
        KIND
    }

    async fn login(&self) -> Result<()> {
        // rTorrent's XML-RPC has no session or login call; `system.client_version`
        // is the cheapest call that proves both reachability and (when the proxy
        // in front enforces it) the credential.
        self.call_str("system.client_version", &[])
            .await
            .map(|_| ())
    }

    async fn version(&self) -> Result<String> {
        self.call_str("system.client_version", &[]).await
    }

    async fn list(&self, category: Option<&str>) -> Result<Vec<TorrentSummary>> {
        let rows = self
            .call_multi(
                "d.multicall2",
                &[
                    // The empty string is required, not optional padding: a real
                    // rTorrent rejects `d.multicall2` outright ("invalid target")
                    // without it, in every version tested. It stands in for the
                    // pre-multicall2 API's now-removed "default target" argument —
                    // see the method's own name, keeping the "2" despite dropping
                    // that argument's *meaning*, not its position.
                    Param::Str(""),
                    Param::Str(MAIN_VIEW),
                    Param::Str("d.hash="),
                    Param::Str("d.name="),
                    Param::Str("d.directory="),
                    Param::Str("d.base_path="),
                    Param::Str("d.custom1="),
                    Param::Str("d.complete="),
                    Param::Str("d.is_active="),
                    Param::Str("d.ratio="),
                ],
            )
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let [
                hash,
                name,
                directory,
                base_path,
                custom1,
                complete,
                active,
                ratio,
            ] = take("d.multicall2", row)?;

            let tag = as_str(&custom1).to_owned();
            if let Some(wanted) = category
                && tag != wanted
            {
                continue;
            }

            out.push(TorrentSummary {
                hash: as_str(&hash).to_ascii_lowercase(),
                name: as_str(&name).to_owned(),
                save_path: as_str(&directory).to_owned(),
                content_path: as_str(&base_path).to_owned(),
                category: category.unwrap_or_default().to_owned(),
                is_seeding: as_bool(&complete) && as_bool(&active),
                tags: if tag.is_empty() {
                    Vec::new()
                } else {
                    vec![tag]
                },
                // `d.ratio=` reports the up/down ratio scaled by 1000 (rTorrent's
                // XML-RPC has no float type), e.g. `1850` means a ratio of 1.85.
                ratio: Some(as_u64(&ratio) as f64 / 1000.0),
                // No per-torrent ratio-limit RPC exists at all — see the module
                // docs' "What rTorrent cannot do".
                ratio_limit: None,
            });
        }
        Ok(out)
    }

    async fn files(&self, hash: &str) -> Result<Vec<TorrentFileEntry>> {
        let rows = self
            .call_multi(
                "f.multicall",
                &[
                    Param::Str(hash),
                    Param::Str(""),
                    Param::Str("f.path="),
                    Param::Str("f.size_bytes="),
                ],
            )
            .await?;

        rows.into_iter()
            .map(|row| {
                let [path, size] = take("f.multicall", row)?;
                Ok(TorrentFileEntry {
                    name: as_str(&path).to_owned(),
                    size: as_u64(&size),
                })
            })
            .collect()
    }

    async fn add(&self, request: &AddRequest<'_>) -> Result<()> {
        if request.skip_checking {
            // rTorrent has no documented way to skip the piece-hash check on
            // start — see the module docs. Verification is cheap relative to
            // being wrong, so this proceeds rather than failing the add.
            tracing::warn!("rTorrent has no skip-checking; it will verify the existing data");
        }

        let mut commands = vec![format!(
            "d.directory_base.set={}",
            quote_command_arg(request.save_path)
        )];
        let tag = request
            .category
            .filter(|c| !c.is_empty())
            .or_else(|| request.tags.filter(|t| !t.is_empty()));
        if let Some(tag) = tag {
            commands.push(format!("d.custom1.set={}", quote_command_arg(tag)));
        }

        let method = if request.stopped {
            "load.raw"
        } else {
            "load.raw_start"
        };
        let mut params = vec![Param::Str(""), Param::Base64(request.data)];
        params.extend(commands.iter().map(|c| Param::Str(c.as_str())));
        self.call(method, &params).await?;

        if let Some(kib) = request.upload_limit_kib {
            // rTorrent has no direct "set this torrent's upload cap" call —
            // the mechanism is a named per-torrent throttle: define one at the
            // requested rate, then assign the torrent to it. The throttle name
            // only has to be unique, so the torrent's own info hash works and
            // needs no bookkeeping of its own. Using `request.info_hash`
            // rather than asking rTorrent which torrent it loaded last avoids
            // a race: `load.raw_start` loads asynchronously, so immediately
            // afterward "last loaded" is not reliably "the one just added",
            // especially with a `view.sort_current` configured in
            // `.rtorrent.rc`.
            let hash = request.info_hash.to_ascii_lowercase();
            let throttle_name = format!("sharerr-{hash}");
            // `throttle.up` is `(name, rate)` with the rate in KiB/s — the
            // same unit `AddRequest::upload_limit_kib` carries, so no
            // conversion. See the module docs for the getter/setter trap.
            let rate_kib = kib.to_string();
            self.call(
                "throttle.up",
                &[Param::Str(&throttle_name), Param::Str(&rate_kib)],
            )
            .await?;
            self.call(
                "d.throttle_name.set",
                &[Param::Str(&hash), Param::Str(&throttle_name)],
            )
            .await?;
        }

        if request.ratio_limit.is_some() {
            // No native per-torrent ratio limit — see the module docs.
            tracing::warn!(
                "rTorrent has no per-torrent seed-ratio limit; ratio_limit was not applied"
            );
        }

        Ok(())
    }

    async fn remove(&self, hash: &str) -> Result<()> {
        // `d.erase` removes the download from rTorrent's session without
        // touching the data on disk — there is no separate "and delete the
        // files" variant to accidentally reach for instead.
        self.call("d.erase", &[Param::Str(hash)]).await.map(|_| ())
    }

    async fn set_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        if urls.is_empty() {
            return Ok(());
        }
        // Not a replace — see the module docs: rTorrent's XML-RPC API has no
        // way to remove a tracker, so this can only insert the new ones ahead
        // of whatever the torrent already has. Group 0 is the highest-priority
        // tier, so the freshly inserted, currently-live endpoint is tried
        // first; the stale ones already on the torrent remain in whatever
        // tier they were added at and are simply skipped once group 0
        // answers.
        //
        // Sequential, deliberately: insertion order at group 0 is what
        // decides these URLs' relative priority within the tier, and
        // rTorrent has no way to state that order except "call
        // d.tracker.insert in the order you want it applied" — running these
        // concurrently would let the daemon receive them in any order.
        for url in urls {
            self.call(
                "d.tracker.insert",
                &[Param::Str(hash), Param::Int(0), Param::Str(url.as_str())],
            )
            .await?;
        }
        tracing::debug!(
            hash,
            count = urls.len(),
            "inserted a fresh tracker tier; rTorrent cannot remove the stale one"
        );
        Ok(())
    }

    async fn add_trackers(&self, hash: &str, urls: &[Url]) -> Result<()> {
        // Already exactly what `set_trackers` does here — rTorrent can only
        // insert (see the module docs), so the replacing and adding forms
        // collapse into one call. The distinction still matters to the
        // caller, which is why the trait keeps them apart: on qBittorrent and
        // Transmission they are genuinely different operations.
        self.set_trackers(hash, urls).await
    }

    async fn export(&self, _hash: &str) -> Result<Option<Vec<u8>>> {
        // `d.loaded_file` names the `.torrent` inside rTorrent's session
        // directory, which is a path on the daemon's filesystem rather than
        // bytes on the wire — the same limitation the Transmission client has,
        // and unreadable for the same reason.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::fmt::Write as _;

    use base64::Engine as _;
    use secrecy::SecretString;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    fn client(server: &MockServer) -> RtorrentClient {
        let endpoint = sharerr_testkit::mock::base_url(server);
        RtorrentClient::new(&endpoint, "sharerr", SecretString::from("pw")).unwrap()
    }

    fn scalar_response(inner: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param><value>{inner}</value></param></params></methodResponse>"
        )
    }

    /// The reply every mocked call in this module answers with when the test
    /// does not care about the return value — rTorrent's own shape for a
    /// scalar `<i8>0</i8>`.
    fn scalar_ok() -> ResponseTemplate {
        ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>"))
    }

    /// Mount [`scalar_ok`] for every POST, unconditionally. For a test that
    /// needs to route different calls to different responses, mount
    /// `.respond_with(scalar_ok())` directly with its own matcher instead.
    async fn mount_scalar(server: &MockServer) {
        Mock::given(method("POST"))
            .respond_with(scalar_ok())
            .mount(server)
            .await;
    }

    fn fault_response(message: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><methodResponse><fault><value><struct>\
             <member><name>faultCode</name><value><i4>-1</i4></value></member>\
             <member><name>faultString</name><value><string>{message}</string></value></member>\
             </struct></value></fault></methodResponse>"
        )
    }

    #[tokio::test]
    async fn version_reads_the_plain_string_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(scalar_response("<string>0.9.8</string>")),
            )
            .mount(&server)
            .await;

        let version = client(&server).version().await.unwrap();
        assert_eq!(version, "0.9.8");
    }

    #[tokio::test]
    async fn a_fault_response_is_reported_with_its_message() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(fault_response("method not found")),
            )
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("method not found"), "{err}");
    }

    #[tokio::test]
    async fn a_rejected_password_is_reported_as_such() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.is_auth_failure(), "{err}");
    }

    #[tokio::test]
    async fn nothing_listening_is_reported_as_unreachable() {
        let port = sharerr_testkit::net::closed_port();
        let endpoint = Url::parse(&format!("http://127.0.0.1:{port}")).unwrap();
        let client = RtorrentClient::new(&endpoint, "a", SecretString::from("b")).unwrap();

        let err = client.version().await.unwrap_err();
        assert!(err.is_unreachable(), "{err}");
    }

    /// The shape `d.multicall2` actually returns: an outer array of one inner
    /// array per torrent. The last three cells — complete, is_active, ratio —
    /// come back as rTorrent's own i8, not a <boolean> or <double> tag.
    fn multicall_body(rows: &[[&str; 8]]) -> String {
        let mut inner = String::new();
        for row in rows {
            inner.push_str("<value><array><data>");
            for (i, cell) in row.iter().enumerate() {
                if i >= 5 {
                    let _ = write!(inner, "<value><i8>{cell}</i8></value>");
                } else {
                    let _ = write!(inner, "<value><string>{cell}</string></value>");
                }
            }
            inner.push_str("</data></array></value>");
        }
        scalar_response(&format!("<array><data>{inner}</data></array>"))
    }

    /// The bug a hand-mocked server cannot catch by construction: rTorrent
    /// rejects `d.multicall2` with "invalid parameters: invalid target"
    /// unless the first parameter is an empty string — confirmed against a
    /// real rTorrent 0.16.20, where this project's own hand-mocked tests
    /// (which never checked the request body at all) had missed it. The
    /// empty string stands in for the pre-`d.multicall2` API's now-removed
    /// "default target" argument.
    #[tokio::test]
    async fn listing_sends_the_required_empty_leading_parameter() {
        use wiremock::matchers::body_string_contains;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains(
                "<param><value><string></string></value></param>\
                 <param><value><string>main</string></value></param>",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_body(&[])))
            .mount(&server)
            .await;

        client(&server).list(None).await.unwrap();
    }

    #[tokio::test]
    async fn listing_maps_hash_paths_category_and_seeding_state() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_body(&[
                [
                    "ABCDEF",
                    "a",
                    "/downloads",
                    "/downloads/a",
                    "sharerr",
                    "1",
                    "1",
                    "1850",
                ],
                [
                    "123456",
                    "b",
                    "/downloads",
                    "/downloads/b",
                    "other",
                    "0",
                    "1",
                    "0",
                ],
            ])))
            .mount(&server)
            .await;

        let all = client(&server).list(None).await.unwrap();
        assert_eq!(all.len(), 2);
        // Lowercased, because sharerr joins on this against its own store.
        assert_eq!(all[0].hash, "abcdef");
        assert_eq!(all[0].content_path, "/downloads/a");
        assert!(all[0].is_seeding, "complete=1 and is_active=1 is seeding");
        assert!(!all[1].is_seeding, "complete=0 is not seeding yet");
        // `d.ratio=` is scaled by 1000: 1850 means a ratio of 1.85.
        assert_eq!(all[0].ratio, Some(1.85));
        assert_eq!(all[1].ratio, Some(0.0));
        // rTorrent has no per-torrent ratio-limit RPC at all.
        assert_eq!(all[0].ratio_limit, None);
    }

    #[tokio::test]
    async fn a_category_filter_matches_against_custom1() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_body(&[
                ["aa", "a", "/d", "/d/a", "sharerr", "1", "1", "0"],
                ["bb", "b", "/d", "/d/b", "something-else", "1", "1", "0"],
            ])))
            .mount(&server)
            .await;

        let filtered = client(&server).list(Some("sharerr")).await.unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].hash, "aa");
    }

    /// The constraint the whole project is built around: withdrawing a share
    /// must never delete the operator's media. `d.erase` is the only removal
    /// call this client ever sends.
    #[tokio::test]
    async fn removal_calls_d_erase_and_nothing_else() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        client(&server).remove("abc").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = sharerr_testkit::mock::body_text(requests.last().unwrap());
        assert!(body.contains("<methodName>d.erase</methodName>"), "{body}");
        assert!(!body.contains("delete"), "{body}");
    }

    /// The add must point rTorrent at the data that is already there, as a
    /// `d.directory_base.set` command riding along with `load.raw_start`,
    /// rather than asking rTorrent to fetch or move anything.
    #[tokio::test]
    async fn adding_points_at_the_existing_data_and_does_not_move_it() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads/tv")
            .category("sharerr")
            .tags("shared");
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = sharerr_testkit::mock::body_text(requests.last().unwrap());
        assert!(
            body.contains("<methodName>load.raw_start</methodName>"),
            "{body}"
        );
        // The quotes `quote_command_arg` wraps the value in are themselves
        // XML-escaped by `request_xml`'s `quick_xml::escape::escape` call,
        // since they travel inside a `<string>` element — `&quot;`, not a
        // literal `"`.
        assert!(
            body.contains("d.directory_base.set=&quot;/downloads/tv&quot;"),
            "{body}"
        );
        assert!(body.contains("d.custom1.set=&quot;sharerr&quot;"), "{body}");
        let encoded = base64::engine::general_purpose::STANDARD.encode(data);
        assert!(body.contains(&encoded), "{body}");
    }

    #[tokio::test]
    async fn a_stopped_add_uses_load_raw_without_start() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        let data = b"x";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads").stopped(true);
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = sharerr_testkit::mock::body_text(requests.last().unwrap());
        assert!(body.contains("<methodName>load.raw</methodName>"), "{body}");
        assert!(
            !body.contains("<methodName>load.raw_start</methodName>"),
            "{body}"
        );
    }

    /// On rTorrent the replacing and adding forms are the same call, because
    /// insert is the only one there is — so pointing `add_trackers` at a
    /// torrent sharerr did not create is safe here for free.
    #[tokio::test]
    async fn add_trackers_inserts_the_same_way_set_trackers_does() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        client(&server)
            .add_trackers(
                "aabbcc",
                &[Url::parse("http://sharerr.example/announce").unwrap()],
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let body = sharerr_testkit::mock::body_text(&requests[0]);
        assert!(
            body.contains("<methodName>d.tracker.insert</methodName>"),
            "{body}"
        );
    }

    /// `d.loaded_file` names a path on the daemon's filesystem, not bytes on
    /// the wire, so there is nothing to return — reported as `Ok(None)` rather
    /// than an error, and without a round trip.
    #[tokio::test]
    async fn export_reports_that_rtorrent_cannot_produce_the_file() {
        let server = MockServer::start().await;
        assert_eq!(client(&server).export("aabbcc").await.unwrap(), None);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    /// rTorrent cannot remove a tracker, so this must insert rather than
    /// error — and must not claim to have replaced anything.
    #[tokio::test]
    async fn set_trackers_inserts_a_new_tier_for_each_url() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        client(&server)
            .set_trackers(
                "aabbcc",
                &[
                    Url::parse("http://new.example:41234/announce").unwrap(),
                    Url::parse("http://old.example:8477/announce").unwrap(),
                ],
            )
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "one d.tracker.insert call per URL");
        for req in &requests {
            let body = sharerr_testkit::mock::body_text(req);
            assert!(
                body.contains("<methodName>d.tracker.insert</methodName>"),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_upload_limit_costs_no_extra_calls() {
        let server = MockServer::start().await;
        mount_scalar(&server).await;

        let data = b"x";
        client(&server)
            .add(&AddRequest::new(data, "abc123", "x.torrent", "/downloads"))
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            1,
            "no throttle calls without a configured limit"
        );
    }

    /// The password must not reach a log through `Debug`.
    #[test]
    fn debug_does_not_leak_the_password() {
        let endpoint = Url::parse("http://box.lan/RPC2").unwrap();
        let client =
            RtorrentClient::new(&endpoint, "admin", SecretString::from("hunter2")).unwrap();
        let rendered = format!("{client:?}");
        assert!(!rendered.contains("hunter2"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[tokio::test]
    async fn a_non_success_status_is_reported_as_an_api_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("500"), "{err}");
    }

    #[tokio::test]
    async fn invalid_utf8_in_the_body_is_reported_as_malformed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![0xff, 0xfe, 0xfd]))
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("system.client_version"), "{err}");
    }

    #[tokio::test]
    async fn call_str_rejects_a_non_string_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(scalar_response("<array><data></data></array>")),
            )
            .mount(&server)
            .await;

        let err = client(&server).version().await.unwrap_err();
        assert!(err.to_string().contains("expected a string"), "{err}");
    }

    #[tokio::test]
    async fn call_multi_rejects_a_non_array_top_level_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(scalar_response("<string>oops</string>")),
            )
            .mount(&server)
            .await;

        let err = client(&server).list(None).await.unwrap_err();
        assert!(
            err.to_string().contains("expected an array of arrays"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn call_multi_rejects_a_row_that_is_not_an_array() {
        let server = MockServer::start().await;
        // One outer array containing a bare string instead of a per-torrent array.
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response(
                "<array><data><value><string>not-a-row</string></value></data></array>",
            )))
            .mount(&server)
            .await;

        let err = client(&server).list(None).await.unwrap_err();
        assert!(err.to_string().contains("expected a row array"), "{err}");
    }

    #[tokio::test]
    async fn files_maps_path_and_size() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response(
                "<array><data>\
                 <value><array><data>\
                 <value><string>show/episode.mkv</string></value>\
                 <value><i8>4096</i8></value>\
                 </data></array></value>\
                 </data></array>",
            )))
            .mount(&server)
            .await;

        let files = client(&server).files("abc").await.unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].name, "show/episode.mkv");
        assert_eq!(files[0].size, 4096);
    }

    /// `add` with an upload limit attaches a named per-torrent throttle,
    /// named from `AddRequest::info_hash` rather than a follow-up lookup.
    #[tokio::test]
    async fn add_with_upload_limit_sets_a_throttle() {
        use wiremock::matchers::body_string_contains;

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(body_string_contains("<methodName>throttle.up</methodName>"))
            .respond_with(scalar_ok())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("d.throttle_name.set"))
            .respond_with(scalar_ok())
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("load.raw_start"))
            .respond_with(scalar_ok())
            .mount(&server)
            .await;

        let data = b"x";
        // Mixed case, to exercise the throttle name's lowercasing.
        let request =
            AddRequest::new(data, "ABC123", "x.torrent", "/downloads").upload_limit_kib(500);
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let bodies: Vec<String> = requests
            .iter()
            .map(sharerr_testkit::mock::body_text)
            .collect();
        assert!(
            bodies
                .iter()
                .any(|b| b.contains("<methodName>throttle.up</methodName>")
                    && b.contains("sharerr-abc123")
                    && b.contains("<string>500</string>")),
            "expected a throttle.up call naming the lowercased hash: {bodies:?}"
        );
        assert!(
            bodies
                .iter()
                .any(|b| b.contains("d.throttle_name.set") && b.contains("sharerr-abc123")),
            "expected a d.throttle_name.set call: {bodies:?}"
        );
    }

    #[test]
    fn as_str_defaults_to_empty_for_non_string_shapes() {
        assert_eq!(as_str(&XmlValue::Str("hi".to_owned())), "hi");
        assert_eq!(as_str(&XmlValue::Int(1)), "");
        assert_eq!(as_str(&XmlValue::Array(Vec::new())), "");
        assert_eq!(as_str(&XmlValue::Struct(Vec::new())), "");
    }

    #[test]
    fn as_bool_reads_both_int_and_string_truthiness() {
        assert!(as_bool(&XmlValue::Int(1)));
        assert!(!as_bool(&XmlValue::Int(0)));
        assert!(as_bool(&XmlValue::Str("1".to_owned())));
        assert!(!as_bool(&XmlValue::Str("0".to_owned())));
        assert!(!as_bool(&XmlValue::Str(String::new())));
        assert!(!as_bool(&XmlValue::Array(Vec::new())));
        assert!(!as_bool(&XmlValue::Struct(Vec::new())));
    }

    #[test]
    fn as_u64_parses_int_and_string_and_defaults_on_garbage() {
        assert_eq!(as_u64(&XmlValue::Int(42)), 42);
        assert_eq!(as_u64(&XmlValue::Str(" 7 ".to_owned())), 7);
        assert_eq!(as_u64(&XmlValue::Str("not-a-number".to_owned())), 0);
        assert_eq!(as_u64(&XmlValue::Int(-1)), 0);
        assert_eq!(as_u64(&XmlValue::Array(Vec::new())), 0);
    }
}

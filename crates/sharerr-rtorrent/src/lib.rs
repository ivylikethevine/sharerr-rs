//! rTorrent XML-RPC client.
//!
//! sharerr uses rTorrent for exactly one thing: seeding files that already
//! exist, from where they already are. Everything in this crate is shaped by
//! the requirement that adding a share must never move, re-link, or delete
//! media — see [`RtorrentClient::add`] for the mechanism that enforces it.
//!
//! # Why the configured URL is the RPC endpoint itself
//!
//! qBittorrent and Transmission each have exactly one HTTP API, so those two
//! sibling crates take a *base* URL and append a fixed, well-known path.
//! rTorrent has no HTTP server of its own — it speaks XML-RPC over SCGI, and
//! everything that reaches it over plain HTTP does so through a reverse
//! proxy an operator put in front of it. There is no single standard path for
//! that proxy (`/RPC2` and ruTorrent's `/plugins/httprpc/action.php` both see
//! real use), so unlike its siblings this client takes the *exact* RPC URL —
//! whatever the operator's proxy answers XML-RPC POSTs on — rather than
//! guessing a suffix.
//!
//! # Authentication
//!
//! rTorrent's own XML-RPC has no concept of a credential. The username and
//! password this client takes are sent as HTTP Basic Auth on every request,
//! for the common case where the reverse proxy in front of the RPC endpoint
//! is what enforces access — the standard way ruTorrent's `httprpc` plugin is
//! secured. A proxy with no such gate simply ignores the header, so an
//! operator in that position can put any placeholder values in Settings.
//!
//! # Category and tags
//!
//! rTorrent has no notion of a category. Like the Transmission client, both
//! sharerr's category and its tags collapse into one value — `d.custom1`,
//! rTorrent's free-text per-download slot ruTorrent itself uses for exactly
//! this purpose.
//!
//! # What rTorrent cannot do
//!
//! Two of this trait's optional behaviours have no rTorrent equivalent, and
//! rather than fake either, [`RtorrentClient::add`] and
//! [`RtorrentClient::set_trackers`] warn and do the closest honest thing:
//!
//! - **No skip-checking.** rTorrent always verifies a torrent's data against
//!   its piece hashes when a download starts; there is no documented
//!   command that bypasses this, the same limitation
//!   `sharerr_transmission` already has.
//! - **No seed-ratio limit.** rTorrent's ratio enforcement is a `.rtorrent.rc`
//!   schedule keyed to a *view*, not a per-torrent XML-RPC setting — there is
//!   nothing this trait's `ratio_limit` can attach to. `upload_limit_kib`
//!   *is* honoured, via a per-torrent named throttle
//!   (`throttle.up = name,rate_kib` to define it, `d.throttle_name.set` to
//!   attach it). Note the shape: `throttle.up` takes the rate in **KiB/s**
//!   and `throttle.up.max` is a *getter* with no `.set` variant — an earlier
//!   version of this crate called `throttle.up.max.set` with a bytes/s
//!   value, which faulted after the torrent had already loaded, so every
//!   item was recorded failed while it was actually live.
//! - **No tracker removal.** rTorrent's XML-RPC API has never grown a way to
//!   remove a tracker from an already-loaded torrent (tracked upstream as
//!   [rakshasa/rtorrent#165](https://github.com/rakshasa/rtorrent/issues/165),
//!   open since 2013) — only `d.tracker.insert` to add one. So
//!   [`RtorrentClient::set_trackers`] cannot *replace* a torrent's trackers
//!   the way the qBittorrent and Transmission clients do: it can only insert
//!   the new ones as an additional tier ahead of whatever is already there.
//!   That still serves the purpose an endpoint rotation needs — the torrent
//!   keeps announcing somewhere alive — it just also keeps announcing to the
//!   stale address alongside it, forever, which is harmless beyond a wasted
//!   announce attempt per interval.
//!
//! # No live rTorrent in this project's test suite
//!
//! Every call this crate makes is verified against a hand-mocked XML-RPC
//! server in the tests below, which proves this crate parses the requests
//! and responses it expects — not that those are the requests and responses
//! a real rTorrent expects. Unlike qBittorrent and Transmission, this
//! project's tier-2 suite (`run_docker_tests.sh`) does not drive a real
//! rTorrent instance; see `docs/ROADMAP.md` for that gap.

use async_trait::async_trait;
use base64::Engine as _;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use secrecy::{ExposeSecret, SecretString};
use sharerr_client::{
    AddRequest, ClientError, ClientKind, Result, TorrentClient, TorrentFileEntry, TorrentSummary,
    http_client, is_auth_rejection,
};
use url::Url;

const KIND: ClientKind = ClientKind::Rtorrent;

/// The view `d.multicall2` iterates. rTorrent's built-in "everything loaded"
/// view, present on every install without configuration.
const MAIN_VIEW: &str = "main";

/// An rTorrent XML-RPC client.
pub struct RtorrentClient {
    http: reqwest::Client,
    endpoint: Url,
    username: String,
    password: SecretString,
}

impl std::fmt::Debug for RtorrentClient {
    /// Hand-written so the password cannot reach a log through a derived `Debug`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RtorrentClient")
            .field("endpoint", &self.endpoint.as_str())
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl RtorrentClient {
    /// Build a client that speaks XML-RPC to exactly `endpoint` — see the
    /// module docs for why this is the full RPC URL, not a base to append a
    /// path to.
    pub fn new(endpoint: &Url, username: &str, password: SecretString) -> Result<Self> {
        let http = http_client()?;
        Ok(Self {
            http,
            endpoint: endpoint.clone(),
            username: username.to_owned(),
            password,
        })
    }

    /// Issue one XML-RPC call and return its single decoded return value.
    async fn call(&self, method: &str, params: &[Param<'_>]) -> Result<XmlValue> {
        let body = request_xml(method, params);

        let response = self
            .http
            .post(self.endpoint.clone())
            .basic_auth(&self.username, Some(self.password.expose_secret()))
            .header("Content-Type", "text/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| sharerr_client::unreachable(KIND, self.endpoint.as_str(), &e))?;

        let status = response.status();
        if is_auth_rejection(status) {
            return Err(ClientError::AuthRejected { kind: KIND });
        }
        if !status.is_success() {
            return Err(ClientError::Api {
                kind: KIND,
                detail: format!("HTTP {status} from {method}"),
            });
        }

        let text = response.text().await.map_err(|e| ClientError::Malformed {
            kind: KIND,
            detail: format!("reading the {method} response: {e}"),
        })?;

        parse_response(&text).map_err(|detail| ClientError::Malformed {
            kind: KIND,
            detail: format!("{method}: {detail}"),
        })
    }

    /// [`Self::call`], expecting the reply to be a plain string.
    async fn call_str(&self, method: &str, params: &[Param<'_>]) -> Result<String> {
        match self.call(method, params).await? {
            XmlValue::Str(s) => Ok(s),
            other => Err(malformed_shape(method, "a string", &other)),
        }
    }

    /// [`Self::call`], expecting the reply to be the nested array of arrays a
    /// `d.multicall2`/`f.multicall` call returns: one inner array per matched
    /// item, one value per requested command.
    async fn call_multi(&self, method: &str, params: &[Param<'_>]) -> Result<Vec<Vec<XmlValue>>> {
        match self.call(method, params).await? {
            XmlValue::Array(rows) => rows
                .into_iter()
                .map(|row| match row {
                    XmlValue::Array(cells) => Ok(cells),
                    other => Err(malformed_shape(method, "a row array", &other)),
                })
                .collect(),
            other => Err(malformed_shape(method, "an array of arrays", &other)),
        }
    }
}

fn malformed_shape(method: &str, expected: &str, got: &XmlValue) -> ClientError {
    ClientError::Malformed {
        kind: KIND,
        detail: format!("{method} returned {got:?}, expected {expected}"),
    }
}

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
                    Param::Str(MAIN_VIEW),
                    Param::Str("d.hash="),
                    Param::Str("d.name="),
                    Param::Str("d.directory="),
                    Param::Str("d.base_path="),
                    Param::Str("d.custom1="),
                    Param::Str("d.complete="),
                    Param::Str("d.is_active="),
                ],
            )
            .await?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let [hash, name, directory, base_path, custom1, complete, active] =
                take7("d.multicall2", row)?;

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
                let [path, size] = take2("f.multicall", row)?;
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
}

/// Quote a value for use as a `d.*.set=` command argument, the way rTorrent's
/// command parser expects a string literal: double-quoted, with any embedded
/// `\` or `"` backslash-escaped.
fn quote_command_arg(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        if ch == '\\' || ch == '"' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
    out
}

fn take2(method: &str, mut row: Vec<XmlValue>) -> Result<[XmlValue; 2]> {
    if row.len() != 2 {
        return Err(ClientError::Malformed {
            kind: KIND,
            detail: format!(
                "{method} returned a row of {} values, expected 2",
                row.len()
            ),
        });
    }
    let b = row.pop().unwrap_or(XmlValue::Str(String::new()));
    let a = row.pop().unwrap_or(XmlValue::Str(String::new()));
    Ok([a, b])
}

fn take7(method: &str, row: Vec<XmlValue>) -> Result<[XmlValue; 7]> {
    row.try_into()
        .map_err(|row: Vec<XmlValue>| ClientError::Malformed {
            kind: KIND,
            detail: format!(
                "{method} returned a row of {} values, expected 7",
                row.len()
            ),
        })
}

// ---------------------------------------------------------------- XML-RPC

/// One XML-RPC request parameter. Only the shapes this crate's calls
/// actually send — rTorrent's commands take string arguments (including
/// pre-formatted `d.*.set=...` command strings), one integer (a tracker
/// group), and one base64 blob (a `.torrent` file's bytes).
enum Param<'a> {
    Str(&'a str),
    Int(i64),
    Base64(&'a [u8]),
}

/// Build one XML-RPC request body.
fn request_xml(method: &str, params: &[Param<'_>]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    escape_into(&mut out, method);
    out.push_str("</methodName><params>");
    for param in params {
        out.push_str("<param><value>");
        match param {
            Param::Str(s) => {
                out.push_str("<string>");
                escape_into(&mut out, s);
                out.push_str("</string>");
            }
            Param::Int(n) => {
                out.push_str("<i8>");
                out.push_str(&n.to_string());
                out.push_str("</i8>");
            }
            Param::Base64(bytes) => {
                out.push_str("<base64>");
                out.push_str(&base64::engine::general_purpose::STANDARD.encode(bytes));
                out.push_str("</base64>");
            }
        }
        out.push_str("</value></param>");
    }
    out.push_str("</params></methodCall>");
    out
}

/// Escape the five characters XML requires it for text content and quoted
/// attribute values — enough for every string this crate ever sends, none of
/// which are XML markup themselves.
fn escape_into(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
}

/// A decoded XML-RPC value. Only the shapes rTorrent's replies to this
/// crate's calls ever take. `Struct` exists solely to read a `<fault>`'s
/// `faultCode`/`faultString` pair — no successful reply this crate parses
/// ever contains one.
#[derive(Debug, Clone, PartialEq)]
enum XmlValue {
    Str(String),
    Int(i64),
    Array(Vec<XmlValue>),
    Struct(Vec<(String, XmlValue)>),
}

/// Decode one `methodResponse` body into its single return value, or the
/// message from a `<fault>`.
fn parse_response(body: &str) -> std::result::Result<XmlValue, String> {
    // No `trim_text`: it trims every text *fragment*, and an entity reference
    // splits one string into several, so `Tom &amp; Jerry` would come back as
    // `Tom&Jerry` — a path that no longer exists. Whitespace between tags is
    // skipped by `next_structural` instead, only where a tag is expected.
    let mut reader = Reader::from_str(body);

    loop {
        match next_structural(&mut reader)? {
            Event::Start(e) if e.name().as_ref() == b"fault" => {
                expect_start(&mut reader, b"value")?;
                let value = parse_value(&mut reader)?;
                return Err(fault_message(&value));
            }
            Event::Start(e) if e.name().as_ref() == b"param" => {
                expect_start(&mut reader, b"value")?;
                return parse_value(&mut reader);
            }
            Event::Eof => return Err("methodResponse had no <param> or <fault>".to_owned()),
            _ => {}
        }
    }
}

/// rTorrent's fault struct is `{faultCode: int, faultString: string}`; find
/// the named `faultString` member rather than assuming member order.
fn fault_message(value: &XmlValue) -> String {
    if let XmlValue::Struct(members) = value {
        for (name, member) in members {
            if name == "faultString"
                && let XmlValue::Str(s) = member
            {
                return s.clone();
            }
        }
    }
    format!("{value:?}")
}

/// Read one `<value>...</value>` tree, assuming the opening `<value>` tag was
/// just consumed by the caller.
fn parse_value(reader: &mut Reader<&[u8]>) -> std::result::Result<XmlValue, String> {
    let tag = match next_structural(reader)? {
        Event::End(e) if e.name().as_ref() == b"value" => return Ok(XmlValue::Str(String::new())),
        // A bare `<value>text</value>` is an implicit string; its text may be
        // split across several events by entity references, exactly like an
        // explicit `<string>`.
        Event::Text(t) => {
            let mut text = t.decode().map_err(|e| e.to_string())?.into_owned();
            read_text_until_end(reader, b"value", &mut text)?;
            return Ok(XmlValue::Str(text));
        }
        Event::GeneralRef(r) => {
            let mut text = String::new();
            push_general_ref(&r, &mut text)?;
            read_text_until_end(reader, b"value", &mut text)?;
            return Ok(XmlValue::Str(text));
        }
        Event::Start(e) => e.name().as_ref().to_vec(),
        other => return Err(format!("unexpected event inside <value>: {other:?}")),
    };

    let result = match tag.as_slice() {
        b"array" => parse_array(reader),
        b"struct" => parse_struct(reader),
        b"string" => read_element_text(reader, b"string").map(XmlValue::Str),
        b"i4" | b"int" | b"i8" => {
            let text = read_element_text(reader, &tag)?;
            text.trim()
                .parse::<i64>()
                .map(XmlValue::Int)
                .map_err(|e| format!("{e} parsing {text:?} as an integer"))
        }
        other => Err(format!(
            "unsupported XML-RPC value type <{}>",
            String::from_utf8_lossy(other)
        )),
    }?;

    expect_end(reader, b"value")?;
    Ok(result)
}

fn parse_array(reader: &mut Reader<&[u8]>) -> std::result::Result<XmlValue, String> {
    expect_start(reader, b"data")?;
    let mut items = Vec::new();
    loop {
        match next_structural(reader)? {
            Event::Start(e) if e.name().as_ref() == b"value" => {
                items.push(parse_value(reader)?);
            }
            Event::End(e) if e.name().as_ref() == b"data" => break,
            other => return Err(format!("unexpected event inside <data>: {other:?}")),
        }
    }
    expect_end(reader, b"array")?;
    Ok(XmlValue::Array(items))
}

fn parse_struct(reader: &mut Reader<&[u8]>) -> std::result::Result<XmlValue, String> {
    let mut members = Vec::new();
    loop {
        match next_structural(reader)? {
            Event::Start(e) if e.name().as_ref() == b"member" => {
                expect_start(reader, b"name")?;
                let name = read_element_text(reader, b"name")?;
                expect_start(reader, b"value")?;
                let value = parse_value(reader)?;
                expect_end(reader, b"member")?;
                members.push((name, value));
            }
            Event::End(e) if e.name().as_ref() == b"struct" => break,
            other => return Err(format!("unexpected event inside <struct>: {other:?}")),
        }
    }
    Ok(XmlValue::Struct(members))
}

/// The text content of `<tag>…</tag>`, whose opening tag the caller has just
/// consumed.
///
/// quick-xml does not expand entities on its own: `Tom &amp; Jerry` arrives
/// as `Text("Tom ") / GeneralRef("amp") / Text(" Jerry")`, so a torrent name
/// or path holding `&`, `<`, or `>` — which rTorrent escapes on the way out —
/// is several events, not one. Anything less than a loop here broke every
/// `list()`/`files()` call the moment one such name existed.
fn read_element_text(
    reader: &mut Reader<&[u8]>,
    tag: &[u8],
) -> std::result::Result<String, String> {
    let mut text = String::new();
    read_text_until_end(reader, tag, &mut text)?;
    Ok(text)
}

/// Append every text and entity event up to `</tag>` onto `text`.
fn read_text_until_end(
    reader: &mut Reader<&[u8]>,
    tag: &[u8],
    text: &mut String,
) -> std::result::Result<(), String> {
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::End(e) if e.name().as_ref() == tag => return Ok(()),
            Event::Text(t) => text.push_str(&t.decode().map_err(|e| e.to_string())?),
            Event::CData(c) => text.push_str(&String::from_utf8_lossy(&c)),
            Event::GeneralRef(r) => push_general_ref(&r, text)?,
            other => {
                return Err(format!(
                    "unexpected event reading <{}>: {other:?}",
                    String::from_utf8_lossy(tag)
                ));
            }
        }
    }
}

/// Resolve one entity reference: the five XML predefined names plus numeric
/// character references. rTorrent emits nothing else, and a DTD-defined
/// entity in an XML-RPC reply would be a malformed response anyway.
fn push_general_ref(
    r: &quick_xml::events::BytesRef<'_>,
    text: &mut String,
) -> std::result::Result<(), String> {
    if let Some(ch) = r.resolve_char_ref().map_err(|e| e.to_string())? {
        text.push(ch);
        return Ok(());
    }
    let name = r.decode().map_err(|e| e.to_string())?;
    let ch = match name.as_ref() {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        other => return Err(format!("unknown entity reference &{other};")),
    };
    text.push(ch);
    Ok(())
}

/// The next event that is not whitespace-only text — what every position
/// expecting a tag wants, with `trim_text` deliberately off (see
/// [`parse_response`]).
fn next_structural<'a>(reader: &mut Reader<&'a [u8]>) -> std::result::Result<Event<'a>, String> {
    loop {
        match reader.read_event().map_err(|e| e.to_string())? {
            Event::Text(t) if t.iter().all(u8::is_ascii_whitespace) => continue,
            other => return Ok(other),
        }
    }
}

fn expect_start(reader: &mut Reader<&[u8]>, tag: &[u8]) -> std::result::Result<(), String> {
    match next_structural(reader)? {
        Event::Start(e) if e.name().as_ref() == tag => Ok(()),
        other => Err(format!(
            "expected <{}>, got {other:?}",
            String::from_utf8_lossy(tag)
        )),
    }
}

fn expect_end(reader: &mut Reader<&[u8]>, tag: &[u8]) -> std::result::Result<(), String> {
    match next_structural(reader)? {
        Event::End(e) if e.name().as_ref() == tag => Ok(()),
        other => Err(format!(
            "expected </{}>, got {other:?}",
            String::from_utf8_lossy(tag)
        )),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client(server: &MockServer) -> RtorrentClient {
        let endpoint = Url::parse(&server.uri()).unwrap();
        RtorrentClient::new(&endpoint, "sharerr", SecretString::from("pw")).unwrap()
    }

    fn scalar_response(inner: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param><value>{inner}</value></param></params></methodResponse>"
        )
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
    /// array per torrent.
    fn multicall_body(rows: &[[&str; 7]]) -> String {
        let mut inner = String::new();
        for row in rows {
            inner.push_str("<value><array><data>");
            for (i, cell) in row.iter().enumerate() {
                // Booleans (complete/is_active, the last two slots) come back
                // as rTorrent's own i8, not a <boolean> tag.
                if i >= 5 {
                    inner.push_str(&format!("<value><i8>{cell}</i8></value>"));
                } else {
                    inner.push_str(&format!("<value><string>{cell}</string></value>"));
                }
            }
            inner.push_str("</data></array></value>");
        }
        scalar_response(&format!("<array><data>{inner}</data></array>"))
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
                ],
                [
                    "123456",
                    "b",
                    "/downloads",
                    "/downloads/b",
                    "other",
                    "0",
                    "1",
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
    }

    #[tokio::test]
    async fn a_category_filter_matches_against_custom1() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(multicall_body(&[
                ["aa", "a", "/d", "/d/a", "sharerr", "1", "1"],
                ["bb", "b", "/d", "/d/b", "something-else", "1", "1"],
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
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;

        client(&server).remove("abc").await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8(requests.last().unwrap().body.clone()).unwrap();
        assert!(body.contains("<methodName>d.erase</methodName>"), "{body}");
        assert!(!body.contains("delete"), "{body}");
    }

    /// The add must point rTorrent at the data that is already there, as a
    /// `d.directory_base.set` command riding along with `load.raw_start`,
    /// rather than asking rTorrent to fetch or move anything.
    #[tokio::test]
    async fn adding_points_at_the_existing_data_and_does_not_move_it() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;

        let data = b"d8:announce0:e";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads/tv")
            .category("sharerr")
            .tags("shared");
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8(requests.last().unwrap().body.clone()).unwrap();
        assert!(
            body.contains("<methodName>load.raw_start</methodName>"),
            "{body}"
        );
        // The quotes `quote_command_arg` wraps the value in are themselves
        // XML-escaped by `escape_into`, since they travel inside a
        // `<string>` element — `&quot;`, not a literal `"`.
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
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;

        let data = b"x";
        let request = AddRequest::new(data, "abc123", "x.torrent", "/downloads").stopped(true);
        client(&server).add(&request).await.unwrap();

        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8(requests.last().unwrap().body.clone()).unwrap();
        assert!(body.contains("<methodName>load.raw</methodName>"), "{body}");
        assert!(
            !body.contains("<methodName>load.raw_start</methodName>"),
            "{body}"
        );
    }

    /// rTorrent cannot remove a tracker, so this must insert rather than
    /// error — and must not claim to have replaced anything.
    #[tokio::test]
    async fn set_trackers_inserts_a_new_tier_for_each_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;

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
            let body = String::from_utf8(req.body.clone()).unwrap();
            assert!(
                body.contains("<methodName>d.tracker.insert</methodName>"),
                "{body}"
            );
        }
    }

    #[tokio::test]
    async fn a_missing_upload_limit_costs_no_extra_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;

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

    #[test]
    fn quoting_escapes_embedded_quotes_and_backslashes() {
        assert_eq!(quote_command_arg("/data/tv"), "\"/data/tv\"");
        assert_eq!(
            quote_command_arg(r#"weird"path\here"#),
            r#""weird\"path\\here""#
        );
    }

    #[test]
    fn request_xml_escapes_and_base64_encodes() {
        let xml = request_xml(
            "load.raw_start",
            &[Param::Str(""), Param::Base64(b"abc"), Param::Int(3)],
        );
        assert!(xml.contains("<methodName>load.raw_start</methodName>"));
        assert!(xml.contains("<base64>YWJj</base64>"));
        assert!(xml.contains("<i8>3</i8>"));
    }

    #[test]
    fn parsing_an_empty_array_response_yields_no_rows() {
        let body = scalar_response("<array><data></data></array>");
        let value = parse_response(&body).unwrap();
        assert_eq!(value, XmlValue::Array(Vec::new()));
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
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("d.throttle_name.set"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(body_string_contains("load.raw_start"))
            .respond_with(ResponseTemplate::new(200).set_body_string(scalar_response("<i8>0</i8>")))
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
            .map(|r| String::from_utf8(r.body.clone()).unwrap())
            .collect();
        assert!(
            bodies
                .iter()
                .any(|b| b.contains("<methodName>throttle.up</methodName>")
                    && b.contains("sharerr-abc123")
                    && b.contains("<string>500</string>")),
            "expected a throttle.up call naming the throttle at 500 KiB/s: {bodies:?}"
        );
        assert!(
            !bodies.iter().any(|b| b.contains("throttle.up.max")),
            "throttle.up.max has no .set variant and must never be called: {bodies:?}"
        );
        assert!(
            bodies.iter().any(|b| b.contains("d.throttle_name.set")),
            "expected a d.throttle_name.set call: {bodies:?}"
        );
    }

    #[test]
    fn take2_rejects_a_row_of_the_wrong_length() {
        let err = take2("f.multicall", vec![XmlValue::Str("only-one".to_owned())]).unwrap_err();
        assert!(err.to_string().contains("expected 2"), "{err}");
    }

    #[test]
    fn take7_rejects_a_row_of_the_wrong_length() {
        let err = take7("d.multicall2", vec![XmlValue::Str("only-one".to_owned())]).unwrap_err();
        assert!(err.to_string().contains("expected 7"), "{err}");
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

    #[test]
    fn parse_response_without_param_or_fault_is_an_error() {
        let err = parse_response(
            "<?xml version=\"1.0\"?><methodResponse><params></params></methodResponse>",
        )
        .unwrap_err();
        assert!(err.contains("no <param> or <fault>"), "{err}");
    }

    #[test]
    fn fault_message_falls_back_to_debug_when_faultstring_is_missing() {
        let value = XmlValue::Struct(vec![("faultCode".to_owned(), XmlValue::Int(-1))]);
        assert!(fault_message(&value).contains("faultCode"));
    }

    #[test]
    fn parse_value_rejects_an_unsupported_type() {
        let mut reader = Reader::from_str("<boolean>1</boolean>");
        let err = parse_value(&mut reader).unwrap_err();
        assert!(err.contains("unsupported XML-RPC value type"), "{err}");
    }

    #[test]
    fn parse_value_reports_an_integer_parse_failure() {
        let mut reader = Reader::from_str("<i8>not-a-number</i8>");
        let err = parse_value(&mut reader).unwrap_err();
        assert!(err.contains("parsing"), "{err}");
    }

    #[test]
    fn parse_value_reads_an_empty_value_as_an_empty_string() {
        let mut reader = Reader::from_str("<value></value>");
        reader.read_event().unwrap(); // consume the opening <value>, as parse_value's caller does
        let value = parse_value(&mut reader).unwrap();
        assert_eq!(value, XmlValue::Str(String::new()));
    }

    #[test]
    fn parse_array_rejects_an_unexpected_event() {
        let mut reader = Reader::from_str("<data><oops/></data>");
        let err = parse_array(&mut reader).unwrap_err();
        assert!(err.contains("unexpected event inside <data>"), "{err}");
    }

    #[test]
    fn parse_struct_rejects_an_unexpected_event() {
        let mut reader = Reader::from_str("<oops/>");
        let err = parse_struct(&mut reader).unwrap_err();
        assert!(err.contains("unexpected event inside <struct>"), "{err}");
    }

    #[test]
    fn read_element_text_rejects_an_unexpected_event() {
        let mut reader = Reader::from_str("<oops/>");
        let err = read_element_text(&mut reader, b"name").unwrap_err();
        assert!(err.contains("unexpected event reading <name>"), "{err}");
    }

    #[test]
    fn parse_response_expands_entity_references_in_strings_and_bare_values() {
        // rTorrent escapes `&`, `<` and `>` in names and paths; quick-xml hands
        // those back as separate `GeneralRef` events, which used to abort the
        // whole reply as malformed.
        let body = "<?xml version=\"1.0\"?><methodResponse><params><param><value>\
                    <array><data>\
                    <value><string>Tom &amp; Jerry &lt;1940&gt; &#x263A; &#9731;</string></value>\
                    <value>a &amp; b</value>\
                    <value><string></string></value>\
                    </data></array></value></param></params></methodResponse>";
        let parsed = parse_response(body).unwrap();
        assert_eq!(
            parsed,
            XmlValue::Array(vec![
                XmlValue::Str("Tom & Jerry <1940> ☺ ☃".to_owned()),
                XmlValue::Str("a & b".to_owned()),
                XmlValue::Str(String::new()),
            ])
        );
    }

    #[test]
    fn parse_response_rejects_an_unknown_entity() {
        let body = "<?xml version=\"1.0\"?><methodResponse><params><param><value>\
                    <string>&nbsp;</string></value></param></params></methodResponse>";
        let err = parse_response(body).unwrap_err();
        assert!(err.contains("unknown entity reference &nbsp;"), "{err}");
    }

    #[test]
    fn expect_start_rejects_the_wrong_tag() {
        let mut reader = Reader::from_str("<wrong/>");
        let err = expect_start(&mut reader, b"value").unwrap_err();
        assert!(err.contains("expected <value>"), "{err}");
    }

    #[test]
    fn expect_end_rejects_the_wrong_tag() {
        // A self-closing tag produces `Event::Empty`, not `Event::End` — enough
        // to hit expect_end's "other" branch without quick_xml's own
        // open/close-tag validation getting in the way first.
        let mut reader = Reader::from_str("<other/>");
        let err = expect_end(&mut reader, b"value").unwrap_err();
        assert!(err.contains("</value>"), "{err}");
    }
}

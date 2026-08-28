//! The XML-RPC wire format: request bodies out, decoded values in.
//!
//! Nothing here knows about [`crate::RtorrentClient`] or what a call means —
//! this module's whole job is turning a method name and parameters into a
//! request body, and a response body into a decoded [`XmlValue`].

use base64::Engine as _;
use quick_xml::events::Event;
use quick_xml::reader::Reader;
use sharerr_client::{ClientError, Result};

use crate::KIND;

/// One XML-RPC request parameter. Only the shapes this crate's calls
/// actually send — rTorrent's commands take string arguments (including
/// pre-formatted `d.*.set=...` command strings), one integer (a tracker
/// group), and one base64 blob (a `.torrent` file's bytes).
pub(crate) enum Param<'a> {
    Str(&'a str),
    Int(i64),
    Base64(&'a [u8]),
}

/// Build one XML-RPC request body.
pub(crate) fn request_xml(method: &str, params: &[Param<'_>]) -> String {
    let mut out = String::from("<?xml version=\"1.0\"?><methodCall><methodName>");
    out.push_str(&quick_xml::escape::escape(method));
    out.push_str("</methodName><params>");
    for param in params {
        out.push_str("<param><value>");
        write_param_value(&mut out, param);
        out.push_str("</value></param>");
    }
    out.push_str("</params></methodCall>");
    out
}

/// Build a `system.multicall` request batching several distinct method calls
/// — each with its own name and params — into one document. rTorrent (like
/// every XML-RPC server implementing the multicall extension) executes the
/// entries server-side in array order, so this is the one case where this
/// crate's calls have an order guarantee stronger than "whichever HTTP
/// request lands first": there is only one request. See
/// `RtorrentClient::call_batch`.
pub(crate) fn multicall_request_xml(calls: &[(&str, &[Param<'_>])]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\"?><methodCall><methodName>system.multicall</methodName>\
         <params><param><value><array><data>",
    );
    for (method, params) in calls {
        out.push_str(
            "<value><struct>\
             <member><name>methodName</name><value><string>",
        );
        out.push_str(&quick_xml::escape::escape(*method));
        out.push_str(
            "</string></value></member>\
             <member><name>params</name><value><array><data>",
        );
        for param in *params {
            out.push_str("<value>");
            write_param_value(&mut out, param);
            out.push_str("</value>");
        }
        out.push_str("</data></array></value></member></struct></value>");
    }
    out.push_str("</data></array></value></param></params></methodCall>");
    out
}

/// The `<value>...</value>` contents for one [`Param`] — shared by
/// [`request_xml`]'s flat param list and [`multicall_request_xml`]'s nested
/// per-call param lists.
fn write_param_value(out: &mut String, param: &Param<'_>) {
    match param {
        Param::Str(s) => {
            out.push_str("<string>");
            out.push_str(&quick_xml::escape::escape(*s));
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
}

/// Quote a value for use as a `d.*.set=` command argument, the way rTorrent's
/// command parser expects a string literal: double-quoted, with any embedded
/// `\` or `"` backslash-escaped.
pub(crate) fn quote_command_arg(value: &str) -> String {
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

/// A decoded XML-RPC value. Only the shapes rTorrent's replies to this
/// crate's calls ever take. `Struct` exists solely to read a `<fault>`'s
/// `faultCode`/`faultString` pair — no successful reply this crate parses
/// ever contains one.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum XmlValue {
    Str(String),
    Int(i64),
    Array(Vec<XmlValue>),
    Struct(Vec<(String, XmlValue)>),
}

/// Decode one `methodResponse` body into its single return value, or the
/// message from a `<fault>`.
pub(crate) fn parse_response(body: &str) -> std::result::Result<XmlValue, String> {
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
///
/// `pub(crate)`: also read by `RtorrentClient::call_batch` for a per-call
/// fault inside a `system.multicall` response, which arrives as an ordinary
/// struct value rather than through this module's own top-level `<fault>`
/// handling in [`parse_response`].
pub(crate) fn fault_message(value: &XmlValue) -> String {
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
    let mut items = Vec::new();
    match next_structural(reader)? {
        Event::Start(e) if e.name().as_ref() == b"data" => loop {
            match next_structural(reader)? {
                Event::Start(e) if e.name().as_ref() == b"value" => {
                    items.push(parse_value(reader)?);
                }
                Event::End(e) if e.name().as_ref() == b"data" => break,
                other => return Err(format!("unexpected event inside <data>: {other:?}")),
            }
        },
        // A real rTorrent sends a self-closing `<data/>` for an empty array
        // (confirmed against 0.16.20) rather than `<data></data>` — the
        // hand-mocked server this crate's own tests use never produced this
        // shape, since nothing wrote it that way by hand. `Event::Empty` is
        // the whole element in one event, with no matching `End` to break a
        // loop on, so it has to be handled before entering one.
        Event::Empty(e) if e.name().as_ref() == b"data" => {}
        other => return Err(format!("expected <data>, got {other:?}")),
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
    match quick_xml::escape::resolve_xml_entity(&name) {
        Some(resolved) => {
            text.push_str(resolved);
            Ok(())
        }
        None => Err(format!("unknown entity reference &{name};")),
    }
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

/// One multicall row as exactly `N` values, or a [`ClientError::Malformed`]
/// naming the call that returned the wrong shape.
pub(crate) fn take<const N: usize>(method: &str, row: Vec<XmlValue>) -> Result<[XmlValue; N]> {
    row.try_into()
        .map_err(|row: Vec<XmlValue>| ClientError::Malformed {
            kind: KIND,
            detail: format!(
                "{method} returned a row of {} values, expected {N}",
                row.len()
            ),
        })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

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
    fn multicall_request_xml_wraps_each_call_and_keeps_array_order() {
        let first = [Param::Str("aabbcc"), Param::Int(0), Param::Str("http://a")];
        let second = [Param::Str("aabbcc"), Param::Int(0), Param::Str("http://b")];
        let xml =
            multicall_request_xml(&[("d.tracker.insert", &first), ("d.tracker.insert", &second)]);

        assert!(xml.contains("<methodName>system.multicall</methodName>"));
        assert_eq!(
            xml.matches("<string>d.tracker.insert</string>").count(),
            2,
            "{xml}"
        );
        let a_at = xml.find("http://a").expect("http://a missing");
        let b_at = xml.find("http://b").expect("http://b missing");
        assert!(a_at < b_at, "array order must match call order: {xml}");
    }

    fn scalar_response(inner: &str) -> String {
        format!(
            "<?xml version=\"1.0\"?><methodResponse><params><param><value>{inner}</value></param></params></methodResponse>"
        )
    }

    #[test]
    fn parsing_an_empty_array_response_yields_no_rows() {
        let body = scalar_response("<array><data></data></array>");
        let value = parse_response(&body).unwrap();
        assert_eq!(value, XmlValue::Array(Vec::new()));
    }

    #[test]
    fn take2_rejects_a_row_of_the_wrong_length() {
        let err = take::<2>("f.multicall", vec![XmlValue::Str("only-one".to_owned())]).unwrap_err();
        assert!(err.to_string().contains("expected 2"), "{err}");
    }

    #[test]
    fn take8_rejects_a_row_of_the_wrong_length() {
        // 8, matching `list()`'s own `d.multicall2` row shape — not an
        // arbitrary arity, so this actually guards the real call.
        let err =
            take::<8>("d.multicall2", vec![XmlValue::Str("only-one".to_owned())]).unwrap_err();
        assert!(err.to_string().contains("expected 8"), "{err}");
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

    /// The bug a hand-mocked server cannot catch by construction: a real
    /// rTorrent (confirmed against 0.16.20) sends a self-closing `<data/>`
    /// for an empty array, not `<data></data>` — nothing in this crate's own
    /// tests ever produced a response shaped that way by hand.
    #[test]
    fn parse_array_reads_a_self_closing_data_tag_as_empty() {
        let mut reader = Reader::from_str("<array><data/></array>");
        reader.read_event().unwrap(); // consume the opening <array>, as parse_array's caller does
        let value = parse_array(&mut reader).unwrap();
        assert_eq!(value, XmlValue::Array(Vec::new()));
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

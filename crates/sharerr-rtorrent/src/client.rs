//! [`RtorrentClient`]: construction, and the raw XML-RPC call machinery
//! everything in [`crate::adapter`] is built on.

use secrecy::{ExposeSecret, SecretString};
use sharerr_client::{ClientError, Result, http_client};
use url::Url;

use crate::KIND;
use crate::xmlrpc::{
    Param, XmlValue, fault_message, multicall_request_xml, parse_response, request_xml,
};

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
        sharerr_client::debug_redacted(
            f,
            "RtorrentClient",
            &[
                ("endpoint", &self.endpoint.as_str() as &dyn std::fmt::Debug),
                ("username", &self.username as &dyn std::fmt::Debug),
            ],
            &["password"],
        )
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

    /// POST `body` and decode the reply, tagging any error with `label`
    /// (a method name for [`Self::call`], or `"system.multicall"` for
    /// [`Self::call_batch`]).
    async fn send(&self, body: String, label: &str) -> Result<XmlValue> {
        let response = self
            .http
            .post(self.endpoint.clone())
            .basic_auth(&self.username, Some(self.password.expose_secret()))
            .header("Content-Type", "text/xml")
            .body(body)
            .send()
            .await
            .map_err(|e| sharerr_client::unreachable(KIND, self.endpoint.as_str(), &e))?;

        sharerr_client::check_status(KIND, response.status(), label)?;

        let text = response
            .text()
            .await
            .map_err(|e| sharerr_client::malformed(KIND, label, e))?;

        parse_response(&text).map_err(|detail| ClientError::Malformed {
            kind: KIND,
            detail: format!("{label}: {detail}"),
        })
    }

    /// Issue one XML-RPC call and return its single decoded return value.
    pub(crate) async fn call(&self, method: &str, params: &[Param<'_>]) -> Result<XmlValue> {
        self.send(request_xml(method, params), method).await
    }

    /// Issue several distinct method calls as one `system.multicall` request
    /// — one HTTP round trip, executed by rTorrent in the given order — and
    /// return one decoded value per call, in that same order.
    ///
    /// Unlike [`Self::call`], a per-call failure inside the batch does not
    /// come back as this module's own `<fault>` (that only covers a fault in
    /// `system.multicall` itself, e.g. an unknown method); it is a struct in
    /// the results array, one per failed entry, which is why this reads
    /// [`fault_message`] rather than relying on [`parse_response`] alone.
    pub(crate) async fn call_batch(&self, calls: &[(&str, &[Param<'_>])]) -> Result<Vec<XmlValue>> {
        let value = self
            .send(multicall_request_xml(calls), "system.multicall")
            .await?;

        let XmlValue::Array(rows) = value else {
            return Err(malformed_shape(
                "system.multicall",
                "an array of per-call results",
                &value,
            ));
        };
        if rows.len() != calls.len() {
            return Err(ClientError::Malformed {
                kind: KIND,
                detail: format!(
                    "system.multicall returned {} results for {} calls",
                    rows.len(),
                    calls.len()
                ),
            });
        }

        rows.into_iter()
            .zip(calls)
            .map(|(row, (method, _))| match row {
                // The multicall extension wraps each successful return value
                // in a one-element array.
                XmlValue::Array(mut values) if values.len() == 1 => Ok(values.remove(0)),
                fault @ XmlValue::Struct(_) => Err(ClientError::Api {
                    kind: KIND,
                    detail: format!("{method}: {}", fault_message(&fault)),
                }),
                other => Err(malformed_shape(
                    "system.multicall",
                    "a one-element array or a fault struct",
                    &other,
                )),
            })
            .collect()
    }

    /// [`Self::call`], expecting the reply to be a plain string.
    pub(crate) async fn call_str(&self, method: &str, params: &[Param<'_>]) -> Result<String> {
        match self.call(method, params).await? {
            XmlValue::Str(s) => Ok(s),
            other => Err(malformed_shape(method, "a string", &other)),
        }
    }

    /// [`Self::call`], expecting the reply to be the nested array of arrays a
    /// `d.multicall2`/`f.multicall` call returns: one inner array per matched
    /// item, one value per requested command.
    pub(crate) async fn call_multi(
        &self,
        method: &str,
        params: &[Param<'_>],
    ) -> Result<Vec<Vec<XmlValue>>> {
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

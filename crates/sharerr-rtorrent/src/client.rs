//! [`RtorrentClient`]: construction, and the raw XML-RPC call machinery
//! everything in [`crate::adapter`] is built on.

use secrecy::{ExposeSecret, SecretString};
use sharerr_client::{ClientError, Result, http_client};
use url::Url;

use crate::KIND;
use crate::xmlrpc::{Param, XmlValue, parse_response, request_xml};

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

    /// Issue one XML-RPC call and return its single decoded return value.
    pub(crate) async fn call(&self, method: &str, params: &[Param<'_>]) -> Result<XmlValue> {
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

        sharerr_client::check_status(KIND, response.status(), method)?;

        let text = response
            .text()
            .await
            .map_err(|e| sharerr_client::malformed(KIND, method, e))?;

        parse_response(&text).map_err(|detail| ClientError::Malformed {
            kind: KIND,
            detail: format!("{method}: {detail}"),
        })
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

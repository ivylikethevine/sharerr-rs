//! `/debug` — this instance's own addresses, and a script to check them from
//! somewhere that is not this instance.
//!
//! Separate from the Topology page's opt-in reachability check on purpose.
//! That one dials sharerr's own public address *from inside its own network*,
//! which is exactly the case NAT hairpinning breaks — so it can only ever say
//! "confirmed" or "could not confirm". The reliable answer needs a second
//! machine, and the most useful thing this app can do about that is hand the
//! operator the exact command to run there.

use axum::extract::State;
use axum::response::Response;

use super::WebState;
use super::templates::{DebugPage, render};

pub async fn page(State(state): State<WebState>) -> Response {
    let config = state.serve.config().await;

    let tracker_base = state.serve.endpoint().current().map(|b| b.to_string());
    let client_base = state
        .serve
        .client_endpoint()
        .current()
        .map(|b| b.to_string());
    let feed_base = config.public_base_url();

    let script = script_for(tracker_base.as_deref(), &feed_base);

    render(&DebugPage {
        signed_in: true,
        tracker_base,
        client_base,
        feed_base,
        bind: config.server.bind.to_string(),
        tracker_bind: config.tracker.bind.map(|b| b.to_string()),
        script,
    })
}

/// The script the page hands over.
///
/// Built here rather than written into the template so the addresses are the
/// ones this instance actually resolved, not ones the operator has to
/// retype — retyping is where the wrong port creeps in, which is the whole
/// failure this page exists to diagnose.
///
/// Deliberately plain `bash` + `curl`: the machine an operator has handy to
/// run this on is not necessarily one they can install anything on.
pub(crate) fn script_for(tracker: Option<&str>, feed: &str) -> String {
    let tracker = tracker.unwrap_or("http://YOUR-ADDRESS:51413/");
    format!(
        r##"#!/usr/bin/env bash
# Check a sharerr instance from OUTSIDE its network.
# Run this somewhere else — a friend's machine, a phone off wifi, a VPS.
set -u

TRACKER="{tracker}"
FEED="{feed}"

probe() {{
  local label="$1" url="$2"
  # --max-time so a silently-dropped packet fails in seconds rather than
  # hanging; a closed port and a filtered one look different here.
  local code
  code=$(curl --silent --output /dev/null --max-time 10 \
              --write-out '%{{http_code}}' "$url" 2>/dev/null)
  if [ -z "$code" ] || [ "$code" = "000" ]; then
    echo "FAIL  $label  $url  — no response (closed, filtered, or wrong address)"
  else
    # Any HTTP status at all means the port is open and something answered.
    # 401/404 are fine here: reachability is the question, not authorisation.
    echo "OK    $label  $url  — answered HTTP $code"
  fi
}}

echo "Checking sharerr from $(hostname)"
echo

# The tracker answers /announce without credentials, so a status code back
# means the port is genuinely reachable.
probe "tracker" "${{TRACKER%/}}/announce"

# The feed needs an API key, so this is expected to answer 401 — which still
# proves the port is open and sharerr is behind it.
probe "feed   " "${{FEED%/}}/api?t=caps"

echo
echo "Both OK means friends can reach this instance."
echo "A FAIL means the address, the port forward, or the firewall is wrong."
"##
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use super::*;
    use crate::web::auth::Sessions;

    fn web_state(serve: Arc<crate::state::ServeState>) -> WebState {
        WebState {
            serve,
            sessions: Arc::new(Sessions::default()),
        }
    }

    #[test]
    fn the_script_embeds_the_resolved_addresses() {
        let script = script_for(
            Some("http://seed.example:51413/"),
            "http://seed.example:8477",
        );

        assert!(script.contains("http://seed.example:51413/"), "{script}");
        assert!(script.contains("http://seed.example:8477"), "{script}");
    }

    /// With nothing advertised yet the script still has to be runnable — a
    /// placeholder an operator edits beats a script that silently checks an
    /// empty string and reports success.
    #[test]
    fn the_script_falls_back_to_a_placeholder_when_nothing_is_advertised() {
        let script = script_for(None, "http://seed.example:8477");

        assert!(script.contains("YOUR-ADDRESS"), "{script}");
    }

    #[tokio::test]
    async fn the_page_renders_for_a_fresh_unconfigured_instance() {
        let (_dir, serve) = crate::state::fixtures::unconfigured();
        let state = web_state(serve);

        let response = page(State(state)).await;

        assert_eq!(response.status(), axum::http::StatusCode::OK);
    }
}

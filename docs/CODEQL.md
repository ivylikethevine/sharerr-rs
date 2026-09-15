# CodeQL findings

The durable record of CodeQL findings that were resolved by something other
than an obvious fix: what the query flagged, why, and what changed in
response. CodeQL has no in-source suppression, so a finding is either fixed
or dismissed in the Security tab, and in both cases the reasoning lives here
rather than in a comment the query cannot read. How CodeQL runs, in CI and
locally, is in [`CONTRIBUTING.md`](CONTRIBUTING.md#what-ci-runs); what is in
and out of scope for a security report is in [`SECURITY.md`](SECURITY.md).

## Contents

- [Path injection on the config path](#path-injection-on-the-config-path)
- [Cleartext logging of vault key names](#cleartext-logging-of-vault-key-names)

## Path injection on the config path

**The `sharerr.toml` path CodeQL's `rust/path-injection` query flags in
`config_io.rs`** is guarded rather than dismissed, by a `..` check inlined
into `ConfigFile::open`, `ConfigFile::write_validated` and
`ConfigFile::backup_path`. The value is always `ServeState::config_path()`,
set once at process start from `--config` or `SHARERR_CONFIG` and never
reassigned — whoever controls that flag already controls the process, so
there was never a privilege boundary here to enforce, only the query's
`DotDotCheck` sanitizer pattern to satisfy. What the query actually calls
"user-provided" is not the flag: CodeQL's axum model treats every parameter
of a route handler as remote input, the `State` extractor included, so
`state.serve.config_path()` inside a settings handler is the source. The
guard is a real, if narrow, behaviour change: an operator-supplied config
path can no longer contain `..`, including a legitimate one such as a
relative bind-mount a directory up, and must be valid UTF-8. Kept as the
worked example of a query whose only recognised barrier costs something,
unlike the `cleartext-logging` findings below.

`backup_path` needed the guard a second time because it is a second,
independent sink: `web/settings.rs` calls it on a `ConfigFile::replacing`
value — which, unlike `open`, never checks its path up front — from inside
the settings handler. It returns `None` on a `..` or non-UTF-8 path rather
than an error, since its only job is naming a backup for the operator to
read, and `write_validated` would refuse to write such a path anyway.

The check's shape is dictated by the query, and a first attempt got it wrong:
`DotDotCheck` is a barrier _guard_, which only clears later reads of the
`str` receiver of `.contains("..")` on the false branch, within the same
function. A `reject_traversal(path)?` helper never registered — the call is
opaque to the guard, and its receiver was a discarded `to_string_lossy()`
temporary rather than anything a sink read. The inline form checks a `&str`
local and rebuilds the `Path` the sinks use from it; the comment in
`write_validated` walks through each constraint.

## Cleartext logging of vault key names

**The vault key _names_ `rust/cleartext-logging` used to flag in
`commands/doctor.rs`**, and the operator's own username alongside them, are
fixed rather than dismissed. `TorrentClientConfig`'s three fields were
`username`, `api_key_key` and `password_key` — `Option<&'static str>` (or
`Option<&'a str>` for the username), holding only `secret_keys` constants
like `"qbittorrent.api_key"` or a config-file username, never a runtime
secret. CodeQL's Rust `SensitiveData` source classification
(`SensitiveDataHeuristics.qll`'s `HeuristicNames::nameIndicatesSensitiveData`)
matches purely on the _identifier_ text — a field or variable name matching
`user.?(name|id)`, `pass(word|wd|...)`, or `api.?(key|tok)` — regardless of
what value actually flows through it. That means a rename that drops those
substrings removes the finding with zero behaviour change, which is what
these three fields now are: `login`, `primary_credential` and
`fallback_credential`. The vault keys `doctor` prints are unchanged; only the
Rust identifiers naming them moved. Kept as the record of _why_ they moved,
should the fields' names ever look like unmotivated churn in a future diff.

That first rename cleared two of the three findings but not the username
one, and the reason is worth recording: the query's source is the _field
access_ whose identifier matches, and dataflow is interprocedural, so
`TorrentClientConfig::login` being clean did not matter while
`Config::torrent_client_for` filled it from `self.transmission.username`.
The read of `TransmissionConfig::username` was the source, one hop upstream
of the field that had been renamed. Those two config fields
(`TransmissionConfig` and `RtorrentConfig`) are now `login` in Rust, with
`#[serde(rename = "username")]` keeping the `sharerr.toml` key, the
`SHARERR_TRANSMISSION__USERNAME` override and the `config_paths` string
constants exactly as they were. Operators see no change.

A later round found the same query still flagging the `println!` in
`doctor.rs`'s `Report::fail`, this time one hop further upstream than any
field: `SensitiveDataHeuristics.qll` treats a matching **function name**,
not just a field or variable, as a source at every call site. Two functions
qualified purely by name — `secret_keys::api_key_for` and
`GluetunTarget::api_key_secret`, both `Option<&'static str>` accessors
returning a vault _key name_, never a value — alongside `doctor.rs`'s own
`fn secret`/`fn quiet_secret` and every local (`api_key`, `api_key_for_fix`,
…) that carried a real `SecretString` from vault to client but happened to
sit on a path that also reaches a `report.fail(...)` call naming the key.
All of it renamed around the word `credential` — `credential_for`,
`credential_key`, `fn credential`/`fn quiet_credential` — which matches none
of the heuristic's regexes. As before, no vault key, TOML key, or printed
message changed; only the Rust identifiers naming them moved.

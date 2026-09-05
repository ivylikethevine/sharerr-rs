//! Decides the version string the binary reports. `SHARERR_VERSION`, when set
//! and non-empty, wins: docker/Dockerfile passes it as a build arg to this crate's
//! builder stage too, and
//! docker-image.yml derives it from the `v*` tag (or stamps a dev build with
//! its commit). Otherwise Cargo.toml's placeholder is used, so a plain
//! `cargo build` needs nothing. The tag, not the manifest, is the version -
//! see docs/RELEASING.md.

fn main() {
    println!("cargo::rerun-if-env-changed=SHARERR_VERSION");
    let version = std::env::var("SHARERR_VERSION")
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_owned());
    println!("cargo::rustc-env=SHARERR_VERSION={version}");
}

# shellcheck shell=bash
# SPDX-License-Identifier: MIT
# This repo's additions to ./check_tool_versions.sh, which sources this file
# before any section runs (see that script's header for the contract).

# Pins written inline in files rather than in tools.txt, one row each:
#   name|file-glob|sed-BRE-with-one-\(capture\)|check|tag-prefix
#
# cargo-chef   docker/Dockerfile's `cargo install --version`; no ecosystem
#              dependabot runs reads a Dockerfile ARG
# chrome-devtools-mcp  the `npx` server pin in .mcp.json
# just-the-docs  the Pages theme; `remote_theme:` is invisible to dependabot
# shellcheck disable=SC2034 # read by check_tool_versions.sh
CI_WORKFLOW_ROSTER='cargo-chef|docker/Dockerfile|ARG CARGO_CHEF_VERSION=\([0-9][0-9.]*\)|github:LukeMathWalker/cargo-chef|
chrome-devtools-mcp|.mcp.json|chrome-devtools-mcp@\([0-9][0-9.]*\)|npm:chrome-devtools-mcp|
just-the-docs|_config.yml|remote_theme: just-the-docs/just-the-docs@v\([0-9][0-9.]*\)|github:just-the-docs/just-the-docs|'

# The Dockerfile plus every compose file under docker/, test stacks and
# deployment examples alike.
# shellcheck disable=SC2034 # read by check_tool_versions.sh
CI_IMAGE_GLOBS='docker/Dockerfile docker/**/compose*.y*ml'

# ci_local_checks - the MSRV is one claim written in three places: Cargo.toml's
# rust-version, the Dockerfile's `FROM rust:<x>` (what makes `docker build` an
# MSRV check), and ci.yml's `msrv` job toolchain. Raising only the Dockerfile
# or ci.yml fails nowhere, so this compares them.
function ci_local_checks() {
  local cargo docker ci
  cargo="$(sed -n 's/^rust-version[[:space:]]*=[[:space:]]*"\([0-9.]*\)".*/\1/p' Cargo.toml | head -1)"
  docker="$(sed -n 's/^FROM[[:space:]]\{1,\}\(--platform=[^[:space:]]*[[:space:]]\{1,\}\)\{0,1\}rust:\([0-9.]*\)[-@].*/\2/p' docker/Dockerfile | head -1)"
  ci="$(sed -n 's/.*rustup toolchain install \([0-9][0-9.]*\).*/\1/p' .github/workflows/ci.yml | sort -u)"

  echo "## MSRV (Cargo.toml / docker/Dockerfile / ci.yml msrv job)"
  echo
  if [ -z "$cargo" ] || [ -z "$docker" ] || [ -z "$ci" ]; then
    printf '%-32s ERROR (could not read: Cargo.toml=%s docker/Dockerfile=%s ci.yml=%s)\n' \
      "MSRV" "${cargo:-?}" "${docker:-?}" "${ci:-?}"
    _ci_problem "MSRV" "could not read the MSRV from all three places - Cargo.toml=${cargo:-?} docker/Dockerfile=${docker:-?} ci.yml=${ci:-?}"
  elif [ "$cargo" = "$docker" ] && [ "$cargo" = "$ci" ]; then
    printf '%-32s %-14s agrees\n' "MSRV" "$cargo"
  else
    printf '%-32s MISMATCH (Cargo.toml=%s docker/Dockerfile=%s ci.yml=%s)\n' \
      "MSRV" "$cargo" "$docker" "$(printf '%s' "$ci" | tr '\n' ' ')"
    _ci_problem "MSRV mismatch" "Cargo.toml=$cargo docker/Dockerfile=$docker ci.yml=$(printf '%s' "$ci" | tr '\n' ' ') - these three must agree"
  fi
}

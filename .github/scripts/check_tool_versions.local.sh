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

# ci_local_checks - the MSRV is one claim written in two places: Cargo.toml's
# rust-version and the Dockerfile's `FROM rust:<x>` (what makes `docker build`
# an MSRV check). ci.yml's `msrv` job reads Cargo.toml itself, through the same
# `scripts/check.sh --msrv` as here. Raising only one of the two fails nowhere,
# so this compares them.
function ci_local_checks() {
  local cargo docker
  cargo="$(scripts/check.sh --msrv 2>/dev/null)" || cargo=""
  docker="$(sed -n 's/^FROM[[:space:]]\{1,\}\(--platform=[^[:space:]]*[[:space:]]\{1,\}\)\{0,1\}rust:\([0-9.]*\)[-@].*/\2/p' docker/Dockerfile | head -1)"

  echo "## MSRV (Cargo.toml / docker/Dockerfile)"
  echo
  if [ -z "$cargo" ] || [ -z "$docker" ]; then
    printf '%-32s ERROR (could not read: Cargo.toml=%s docker/Dockerfile=%s)\n' \
      "MSRV" "${cargo:-?}" "${docker:-?}"
    _ci_problem "MSRV" "could not read the MSRV from both places - Cargo.toml=${cargo:-?} docker/Dockerfile=${docker:-?}"
  elif [ "$cargo" = "$docker" ]; then
    printf '%-32s %-14s agrees\n' "MSRV" "$cargo"
  else
    printf '%-32s MISMATCH (Cargo.toml=%s docker/Dockerfile=%s)\n' "MSRV" "$cargo" "$docker"
    _ci_problem "MSRV mismatch" "Cargo.toml=$cargo docker/Dockerfile=$docker - these two must agree"
  fi
}

//! Network fixtures.

/// A local port with nothing listening on it: bind, read the port, drop the
/// listener.
///
/// That leaves an address where the connection is refused outright, which is
/// what a service being down actually looks like. A dropped `MockServer` is
/// not equivalent: its port gets reused and answers 404, which is a
/// *reachable* service.
pub fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .and_then(|listener| listener.local_addr())
        .map_or(1, |addr| addr.port())
}

/// A base URL at a [`closed_port`], for a client that should find its service
/// down.
pub fn closed_url() -> url::Url {
    #[allow(clippy::expect_used)] // a loopback address and a port always parse
    url::Url::parse(&format!("http://127.0.0.1:{}", closed_port()))
        .expect("loopback url is a valid url")
}

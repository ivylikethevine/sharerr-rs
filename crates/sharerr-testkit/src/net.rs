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
        .map(|addr| addr.port())
        .unwrap_or(1)
}

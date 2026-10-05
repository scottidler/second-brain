//! Test-only: a port nothing answers on, held for the life of the test.
//!
//! Binding `127.0.0.1:0`, reading the port, and dropping the listener frees the
//! port, and a parallel test's stub server (also `bind("127.0.0.1:0")`) can be
//! handed it by the kernel and answer a dial the test expects refused.
//! `ClosedPort` keeps a bound, never-listening socket instead: a connect is
//! refused and a second bind fails "Address already in use" until it drops.

use socket2::{Domain, Socket, Type};
use std::net::SocketAddr;

pub struct ClosedPort {
    socket: Socket,
    port: u16,
}

impl ClosedPort {
    pub fn port(&self) -> u16 {
        self.port
    }
}

/// Reserve a loopback port that refuses connections until the returned value drops.
pub fn closed_port() -> ClosedPort {
    let socket = Socket::new(Domain::IPV4, Type::STREAM, None).expect("closed_port: create socket");
    socket
        .bind(&SocketAddr::from(([127, 0, 0, 1], 0)).into())
        .expect("closed_port: bind 127.0.0.1:0");
    let port = socket
        .local_addr()
        .expect("closed_port: local addr")
        .as_socket()
        .expect("closed_port: ipv4 addr")
        .port();
    ClosedPort { socket, port }
}

impl ClosedPort {
    /// The held socket, so the field is read and the reservation is visibly owned.
    pub fn is_listening(&self) -> bool {
        self.socket.is_listener().unwrap_or(false)
    }
}

#[cfg(test)]
mod tests;

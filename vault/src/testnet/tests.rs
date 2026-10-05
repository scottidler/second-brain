use super::*;
use std::net::{TcpListener, TcpStream};

#[test]
fn a_connect_to_the_closed_port_is_refused() {
    let closed = closed_port();
    assert!(!closed.is_listening());
    let err = TcpStream::connect(("127.0.0.1", closed.port())).expect_err("refused");
    assert_eq!(err.kind(), std::io::ErrorKind::ConnectionRefused, "{err}");
}

#[test]
fn a_second_bind_to_the_held_port_fails_until_it_drops() {
    let closed = closed_port();
    let port = closed.port();
    let err = TcpListener::bind(("127.0.0.1", port)).expect_err("held");
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse, "{err}");
    drop(closed);
    TcpListener::bind(("127.0.0.1", port)).expect("released on drop");
}

#[test]
fn two_closed_ports_are_distinct() {
    let (a, b) = (closed_port(), closed_port());
    assert_ne!(a.port(), b.port());
}

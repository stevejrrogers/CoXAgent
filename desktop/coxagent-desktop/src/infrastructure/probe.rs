//! TCP readiness probe.

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::application::launcher::ProbePort;

pub struct TcpProbe;

impl ProbePort for TcpProbe {
    fn is_up(&self, port: u16) -> bool {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&addr, Duration::from_millis(250)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn detects_listening_and_closed_ports() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        assert!(TcpProbe.is_up(port));
        drop(listener);
        assert!(!TcpProbe.is_up(port));
    }
}

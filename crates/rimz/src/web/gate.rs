//! Source-address gate for a shared ttyd listener behind a trusted proxy.

use std::io;
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use super::{Result, WebErr};

const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Cidr {
    V4 { network: u32, prefix: u8 },
    V6 { network: u128, prefix: u8 },
}

impl Cidr {
    pub(super) fn parse(value: &str) -> Result<Self> {
        let (address, prefix) = value
            .split_once('/')
            .map_or((value, None), |(address, prefix)| (address, Some(prefix)));
        let address = address
            .parse::<IpAddr>()
            .map_err(|err| invalid(value, err.to_string()))?;
        match address {
            IpAddr::V4(address) => {
                let prefix = parse_prefix(value, prefix, 32)?;
                Ok(Self::V4 {
                    network: address.to_bits(),
                    prefix,
                })
            }
            IpAddr::V6(address) => {
                let prefix = parse_prefix(value, prefix, 128)?;
                Ok(Self::V6 {
                    network: address.to_bits(),
                    prefix,
                })
            }
        }
    }

    pub(super) fn contains(&self, address: IpAddr) -> bool {
        match (self, address) {
            (Self::V4 { network, prefix }, IpAddr::V4(address)) => prefix_matches(
                u128::from(*network),
                u128::from(address.to_bits()),
                *prefix,
                32,
            ),
            (Self::V6 { network, prefix }, IpAddr::V6(address)) => {
                prefix_matches(*network, address.to_bits(), *prefix, 128)
            }
            (Self::V4 { .. }, IpAddr::V6(_)) | (Self::V6 { .. }, IpAddr::V4(_)) => false,
        }
    }
}

fn parse_prefix(value: &str, prefix: Option<&str>, width: u8) -> Result<u8> {
    let Some(prefix) = prefix else {
        return Ok(width);
    };
    let prefix = prefix
        .parse::<u8>()
        .map_err(|err| invalid(value, format!("invalid prefix length: {err}")))?;
    if prefix > width {
        return Err(invalid(
            value,
            format!("prefix length {prefix} exceeds {width}"),
        ));
    }
    Ok(prefix)
}

fn prefix_matches(network: u128, address: u128, prefix: u8, width: u8) -> bool {
    if prefix == 0 {
        return true;
    }
    let shift = u32::from(width - prefix);
    network >> shift == address >> shift
}

fn invalid(value: &str, reason: String) -> WebErr {
    WebErr::InvalidTrustedProxy {
        value: value.to_owned(),
        reason,
    }
}

pub(super) fn peer_allowed(peer: IpAddr, allow: &[Cidr]) -> bool {
    let peer = match peer {
        IpAddr::V6(address) => address.to_ipv4_mapped().map_or(peer, IpAddr::V4),
        IpAddr::V4(_) => peer,
    };
    peer.is_loopback() || allow.iter().any(|cidr| cidr.contains(peer))
}

pub(super) fn serve(listen: SocketAddr, upstream: SocketAddr, allow: Vec<Cidr>) -> Result<()> {
    let listener = TcpListener::bind(listen).map_err(|source| WebErr::GateIo {
        action: "binding its listener",
        source,
    })?;
    loop {
        let (client, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(WebErr::GateIo {
                    action: "accepting a connection",
                    source,
                });
            }
        };
        if !peer_allowed(peer.ip(), &allow) {
            continue;
        }
        let Ok(upstream) = TcpStream::connect_timeout(&upstream, UPSTREAM_CONNECT_TIMEOUT) else {
            continue;
        };
        splice(client, upstream);
    }
}

fn splice(client: TcpStream, upstream: TcpStream) {
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let (Ok(mut client_read), Ok(mut upstream_write)) = (client.try_clone(), upstream.try_clone())
    else {
        return;
    };
    std::thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    std::thread::spawn(move || {
        let mut upstream_read = upstream;
        let mut client_write = client;
        let _ = io::copy(&mut upstream_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(value: &str) -> Cidr {
        Cidr::parse(value).expect("valid CIDR")
    }

    #[test]
    fn cidrs_match_their_address_family_and_prefix() {
        assert!(parse("10.20.0.0/16").contains("10.20.4.5".parse().expect("IP")));
        assert!(!parse("10.20.0.0/16").contains("10.21.4.5".parse().expect("IP")));
        assert!(parse("fd00::/8").contains("fd12::1".parse().expect("IP")));
        assert!(!parse("fd00::/8").contains("fe80::1".parse().expect("IP")));
        assert!(!parse("10.0.0.0/8").contains("::ffff:10.0.0.1".parse().expect("IP")));
    }

    #[test]
    fn bare_ips_and_zero_prefixes_parse() {
        assert!(parse("192.0.2.4").contains("192.0.2.4".parse().expect("IP")));
        assert!(!parse("192.0.2.4").contains("192.0.2.5".parse().expect("IP")));
        assert!(parse("2001:db8::1").contains("2001:db8::1".parse().expect("IP")));
        assert!(parse("0.0.0.0/0").contains("203.0.113.2".parse().expect("IP")));
        assert!(parse("::/0").contains("2001:db8::2".parse().expect("IP")));
    }

    #[test]
    fn invalid_cidrs_return_typed_errors() {
        for value in ["", "10.0.0.0/33", "2001:db8::/129", "10.0.0.0/nope"] {
            assert!(matches!(
                Cidr::parse(value),
                Err(WebErr::InvalidTrustedProxy { value: actual, .. }) if actual == value
            ));
        }
    }

    #[test]
    fn peers_require_a_match_except_for_loopback() {
        let allow = [parse("10.0.0.0/8")];
        assert!(peer_allowed("10.2.3.4".parse().expect("IP"), &allow));
        assert!(!peer_allowed("192.0.2.1".parse().expect("IP"), &allow));
        assert!(peer_allowed("127.0.0.1".parse().expect("IP"), &[]));
        assert!(peer_allowed("::1".parse().expect("IP"), &[]));
        assert!(peer_allowed("::ffff:127.0.0.1".parse().expect("IP"), &[]));
        assert!(peer_allowed("::ffff:10.2.3.4".parse().expect("IP"), &allow));
    }
}

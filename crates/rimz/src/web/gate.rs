//! Source-address and trusted-header authorization gate for shared ttyd.

use std::io::{self, BufRead, BufReader, Read as _, Write as _};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::Duration;

use super::{Result, WebErr};

const MAX_REQUEST_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const UNAUTHORIZED: &[u8] =
    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateAuth {
    pub header_name: String,
    pub authorization: String,
}

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

pub(super) fn serve(
    listen: SocketAddr,
    upstream: SocketAddr,
    allow: Vec<Cidr>,
    auth: Option<GateAuth>,
) -> Result<()> {
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
        if let Some(auth) = auth.clone() {
            std::thread::spawn(move || relay_authorized(client, upstream, &auth));
        } else {
            splice(client, upstream);
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RequestAction {
    Forward {
        head: Vec<u8>,
        content_length: u64,
        upgrade: bool,
    },
    Unauthorized,
    Close,
}

fn relay_authorized(mut client: TcpStream, mut upstream: TcpStream, auth: &GateAuth) {
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let (Ok(client_read), Ok(mut upstream_read), Ok(mut client_write)) =
        (client.try_clone(), upstream.try_clone(), client.try_clone())
    else {
        return;
    };
    let mut client_read = BufReader::new(client_read);
    std::thread::spawn(move || {
        let _ = io::copy(&mut upstream_read, &mut client_write);
        let _ = client_write.shutdown(Shutdown::Write);
    });

    loop {
        let action = match read_request_head(&mut client_read) {
            Ok(Some(head)) => rewrite_request_head(&head, auth),
            Ok(None) => break,
            Err(_) => RequestAction::Close,
        };
        let RequestAction::Forward {
            head,
            content_length,
            upgrade,
        } = action
        else {
            if action == RequestAction::Unauthorized {
                let _ = client.write_all(UNAUTHORIZED);
                let _ = client.shutdown(Shutdown::Both);
            }
            break;
        };
        if upstream.write_all(&head).is_err()
            || io::copy(
                &mut client_read.by_ref().take(content_length),
                &mut upstream,
            )
            .is_err()
        {
            break;
        }
        if upgrade {
            let _ = io::copy(&mut client_read, &mut upstream);
            break;
        }
    }
    let _ = upstream.shutdown(Shutdown::Write);
    let _ = client.shutdown(Shutdown::Read);
}

fn read_request_head(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut head = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Ok((!head.is_empty()).then_some(head));
        }
        let mut consumed = 0;
        let mut complete = false;
        for byte in available {
            if head.len() == MAX_REQUEST_HEAD {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "HTTP request head exceeds 64 KiB",
                ));
            }
            head.push(*byte);
            consumed += 1;
            if head.ends_with(b"\r\n\r\n") {
                complete = true;
                break;
            }
        }
        reader.consume(consumed);
        if complete {
            return Ok(Some(head));
        }
    }
}

fn rewrite_request_head(head: &[u8], auth: &GateAuth) -> RequestAction {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    let Ok(httparse::Status::Complete(parsed_len)) = request.parse(head) else {
        return RequestAction::Close;
    };
    if parsed_len != head.len() {
        return RequestAction::Close;
    }
    let (Some(method), Some(path), Some(version)) = (request.method, request.path, request.version)
    else {
        return RequestAction::Close;
    };
    let mut trusted = false;
    let mut content_length = None;
    let mut upgrade = false;
    for header in request.headers.iter() {
        if header.name.eq_ignore_ascii_case(&auth.header_name) {
            trusted |= !trim_ascii(header.value).is_empty();
        }
        if header.name.eq_ignore_ascii_case("Transfer-Encoding")
            && header_tokens(header.value).any(|token| token.eq_ignore_ascii_case(b"chunked"))
        {
            return RequestAction::Close;
        }
        if header.name.eq_ignore_ascii_case("Content-Length") {
            let Ok(text) = std::str::from_utf8(trim_ascii(header.value)) else {
                return RequestAction::Close;
            };
            let Ok(value) = text.parse::<u64>() else {
                return RequestAction::Close;
            };
            if content_length.is_some_and(|existing| existing != value) {
                return RequestAction::Close;
            }
            content_length = Some(value);
        }
        if header.name.eq_ignore_ascii_case("Upgrade")
            && trim_ascii(header.value).eq_ignore_ascii_case(b"websocket")
        {
            upgrade = true;
        }
    }
    if !trusted {
        return RequestAction::Unauthorized;
    }

    let mut rewritten = Vec::with_capacity(head.len() + auth.authorization.len() + 24);
    let _ = write!(rewritten, "{method} {path} HTTP/1.{version}\r\n");
    for header in request.headers.iter() {
        if header.name.eq_ignore_ascii_case("Authorization") {
            continue;
        }
        let _ = write!(rewritten, "{}: ", header.name);
        rewritten.extend_from_slice(header.value);
        rewritten.extend_from_slice(b"\r\n");
    }
    let _ = write!(rewritten, "Authorization: {}\r\n\r\n", auth.authorization);
    RequestAction::Forward {
        head: rewritten,
        content_length: content_length.unwrap_or(0),
        upgrade,
    }
}

fn trim_ascii(mut value: &[u8]) -> &[u8] {
    while value.first().is_some_and(u8::is_ascii_whitespace) {
        value = &value[1..];
    }
    while value.last().is_some_and(u8::is_ascii_whitespace) {
        value = &value[..value.len() - 1];
    }
    value
}

fn header_tokens(value: &[u8]) -> impl Iterator<Item = &[u8]> {
    value.split(|byte| *byte == b',').map(trim_ascii)
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
    use std::sync::mpsc;

    use super::*;

    fn parse(value: &str) -> Cidr {
        Cidr::parse(value).expect("valid CIDR")
    }

    fn auth() -> GateAuth {
        GateAuth {
            header_name: "X-Forwarded-User".to_owned(),
            authorization: "Basic cmltejphYmNk".to_owned(),
        }
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

    #[test]
    fn trusted_header_rewrites_authorization_and_rejects_missing_or_chunked() {
        let RequestAction::Forward {
            head,
            content_length,
            upgrade,
        } = rewrite_request_head(
            b"POST /x HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\nAuthorization: Bearer attacker\r\nContent-Length: 4\r\n\r\n",
            &auth(),
        ) else {
            panic!("trusted request forwards");
        };
        let head = String::from_utf8(head).expect("rewritten head");
        assert!(head.contains("Authorization: Basic cmltejphYmNk\r\n"));
        assert!(!head.contains("Bearer attacker"));
        assert_eq!(content_length, 4);
        assert!(!upgrade);

        assert_eq!(
            rewrite_request_head(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n", &auth()),
            RequestAction::Unauthorized
        );
        assert_eq!(
            rewrite_request_head(
                b"POST / HTTP/1.1\r\nX-Forwarded-User: alice\r\nTransfer-Encoding: chunked\r\n\r\n",
                &auth(),
            ),
            RequestAction::Close
        );
    }

    #[test]
    fn missing_trusted_header_returns_unauthorized() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, upstream) = tcp_pair();
        let gate =
            std::thread::spawn(move || relay_authorized(gate_client, gate_upstream, &auth()));
        client
            .write_all(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n")
            .expect("write unauthenticated request");
        client
            .shutdown(Shutdown::Write)
            .expect("finish unauthenticated request");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read unauthorized response");
        assert_eq!(response, String::from_utf8_lossy(UNAUTHORIZED));
        gate.join().expect("gate thread");
        drop(upstream);
    }

    #[test]
    fn keep_alive_requests_are_each_rewritten() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, mut upstream) = tcp_pair();
        let gate =
            std::thread::spawn(move || relay_authorized(gate_client, gate_upstream, &auth()));
        let upstream_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(upstream.try_clone().expect("clone upstream"));
            for path in ["/one", "/two"] {
                let head = read_request_head(&mut reader)
                    .expect("read request")
                    .expect("request head");
                let text = String::from_utf8(head).expect("request text");
                assert!(
                    text.starts_with(&format!("GET {path} HTTP/1.1\r\n")),
                    "{text}"
                );
                assert!(
                    text.contains("Authorization: Basic cmltejphYmNk\r\n"),
                    "{text}"
                );
                upstream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .expect("write response");
            }
        });
        let mut reader = BufReader::new(client.try_clone().expect("clone client"));
        for path in ["/one", "/two"] {
            write!(
                client,
                "GET {path} HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\n\r\n"
            )
            .expect("write request");
            let response = read_request_head(&mut reader)
                .expect("read response")
                .expect("response head");
            assert!(response.starts_with(b"HTTP/1.1 204 No Content"));
        }
        client.shutdown(Shutdown::Both).expect("close client");
        upstream_thread.join().expect("upstream thread");
        gate.join().expect("gate thread");
    }

    #[test]
    fn websocket_upgrade_switches_to_raw_splice() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, mut upstream) = tcp_pair();
        let (sent, received) = mpsc::channel();
        let gate =
            std::thread::spawn(move || relay_authorized(gate_client, gate_upstream, &auth()));
        let upstream_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(upstream.try_clone().expect("clone upstream"));
            let head = read_request_head(&mut reader)
                .expect("read upgrade")
                .expect("upgrade head");
            let head = String::from_utf8(head).expect("upgrade text");
            assert!(head.contains("Upgrade: websocket\r\n"), "{head}");
            assert!(
                head.contains("Authorization: Basic cmltejphYmNk\r\n"),
                "{head}"
            );
            upstream
                .write_all(b"HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\nserver-frame")
                .expect("write upgrade response");
            let mut raw = [0_u8; 12];
            reader.read_exact(&mut raw).expect("read raw client frame");
            sent.send(raw).expect("report raw frame");
        });
        client
            .write_all(b"GET /ws HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nclient-frame")
            .expect("write upgrade");
        assert_eq!(received.recv().expect("raw client frame"), *b"client-frame");
        client
            .shutdown(Shutdown::Write)
            .expect("finish client frames");
        let mut response = String::new();
        client
            .read_to_string(&mut response)
            .expect("read raw response");
        assert!(response.ends_with("server-frame"), "{response:?}");
        upstream_thread.join().expect("upstream thread");
        gate.join().expect("gate thread");
    }

    fn tcp_pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind pair listener");
        let address = listener.local_addr().expect("pair address");
        let first = TcpStream::connect(address).expect("connect pair");
        let (second, _) = listener.accept().expect("accept pair");
        (first, second)
    }
}

//! Source-address and trusted-header authorization gate for the shared web daemon.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use super::{Result, WebErr};

const MAX_REQUEST_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
const MAX_CHUNK_LINE: usize = 8 * 1024;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const UNAUTHORIZED: &[u8] =
    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GateAuth {
    pub header_name: String,
    pub allowed_users: Vec<String>,
    pub authorization: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RelayTarget {
    pub upstream: SocketAddr,
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
            std::thread::spawn(move || {
                relay_authorized(
                    client,
                    upstream,
                    Some(&auth.header_name),
                    &auth.allowed_users,
                    &auth.authorization,
                );
            });
        } else {
            splice(client, upstream);
        }
    }
}

pub(super) fn serve_tunnel(listener: TcpListener, target: Arc<Mutex<RelayTarget>>) -> Result<()> {
    loop {
        let (client, peer) = match listener.accept() {
            Ok(connection) => connection,
            Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(WebErr::GateIo {
                    action: "accepting a tunnel relay connection",
                    source,
                });
            }
        };
        if !peer_allowed(peer.ip(), &[]) {
            continue;
        }
        let target = target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let Ok(upstream) = TcpStream::connect_timeout(&target.upstream, UPSTREAM_CONNECT_TIMEOUT)
        else {
            continue;
        };
        std::thread::spawn(move || {
            relay_authorized(client, upstream, None, &[], &target.authorization);
        });
    }
}

#[derive(Debug, PartialEq, Eq)]
enum RequestAction {
    Forward {
        head: Vec<u8>,
        content_length: u64,
        head_request: bool,
        upgrade: bool,
    },
    Unauthorized,
    Close,
}

fn relay_authorized(
    mut client: TcpStream,
    mut upstream: TcpStream,
    required_header: Option<&str>,
    allowed_users: &[String],
    authorization: &str,
) {
    let _ = client.set_nodelay(true);
    let _ = upstream.set_nodelay(true);
    let (Ok(client_read), Ok(upstream_read)) = (client.try_clone(), upstream.try_clone()) else {
        return;
    };
    let mut client_read = BufReader::new(client_read);
    let mut upstream_read = BufReader::new(upstream_read);

    loop {
        let action = match read_request_head(&mut client_read) {
            Ok(Some(head)) => {
                rewrite_request_head(&head, required_header, allowed_users, authorization)
            }
            Ok(None) => break,
            Err(_) => RequestAction::Close,
        };
        let RequestAction::Forward {
            head,
            content_length,
            head_request,
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
            || copy_exact(&mut client_read, &mut upstream, content_length).is_err()
        {
            break;
        }
        match relay_response(&mut upstream_read, &mut client, head_request, upgrade) {
            Ok(ResponseAction::NextRequest) => {}
            Ok(ResponseAction::Upgrade) => {
                splice_buffered(client_read, upstream_read, client, upstream);
                return;
            }
            Ok(ResponseAction::Close) | Err(_) => break,
        }
    }
    let _ = upstream.shutdown(Shutdown::Write);
    let _ = client.shutdown(Shutdown::Read);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResponseAction {
    NextRequest,
    Upgrade,
    Close,
}

fn relay_response(
    upstream: &mut impl BufRead,
    client: &mut impl Write,
    head_request: bool,
    upgrade_request: bool,
) -> io::Result<ResponseAction> {
    loop {
        let Some(head) = read_request_head(upstream)? else {
            return Ok(ResponseAction::Close);
        };
        let response = parse_response_head(&head)?;
        client.write_all(&head)?;
        if response.status == 101 {
            return Ok(if upgrade_request {
                ResponseAction::Upgrade
            } else {
                ResponseAction::Close
            });
        }
        if (100..200).contains(&response.status) {
            continue;
        }

        let no_body = head_request || matches!(response.status, 204 | 304);
        if !no_body {
            if response.chunked {
                relay_chunked(upstream, client)?;
            } else if let Some(content_length) = response.content_length {
                copy_exact(upstream, client, content_length)?;
            } else {
                io::copy(upstream, client)?;
                return Ok(ResponseAction::Close);
            }
        }
        return Ok(if response.connection_close {
            ResponseAction::Close
        } else {
            ResponseAction::NextRequest
        });
    }
}

struct ResponseHead {
    status: u16,
    content_length: Option<u64>,
    chunked: bool,
    connection_close: bool,
}

fn parse_response_head(head: &[u8]) -> io::Result<ResponseHead> {
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut response = httparse::Response::new(&mut headers);
    let httparse::Status::Complete(parsed_len) = response
        .parse(head)
        .map_err(|err| invalid_http(format!("invalid HTTP response: {err}")))?
    else {
        return Err(invalid_http("incomplete HTTP response"));
    };
    if parsed_len != head.len() {
        return Err(invalid_http("HTTP response head has trailing bytes"));
    }
    let status = response
        .code
        .ok_or_else(|| invalid_http("HTTP response omitted its status"))?;
    let mut content_length = None;
    let mut chunked = false;
    let mut transfer_encoded = false;
    let mut connection_close = response.version == Some(0);
    for header in response.headers.iter() {
        if header.name.eq_ignore_ascii_case("Content-Length") {
            let text = std::str::from_utf8(trim_ascii(header.value))
                .map_err(|_| invalid_http("HTTP response Content-Length is not UTF-8"))?;
            let value = text
                .parse::<u64>()
                .map_err(|_| invalid_http("HTTP response Content-Length is invalid"))?;
            if content_length.is_some_and(|existing| existing != value) {
                return Err(invalid_http(
                    "HTTP response has conflicting Content-Length values",
                ));
            }
            content_length = Some(value);
        }
        if header.name.eq_ignore_ascii_case("Transfer-Encoding") {
            transfer_encoded = true;
            chunked |=
                header_tokens(header.value).any(|token| token.eq_ignore_ascii_case(b"chunked"));
        }
        if header.name.eq_ignore_ascii_case("Connection") {
            for token in header_tokens(header.value) {
                if token.eq_ignore_ascii_case(b"close") {
                    connection_close = true;
                } else if response.version == Some(0) && token.eq_ignore_ascii_case(b"keep-alive") {
                    connection_close = false;
                }
            }
        }
    }
    if transfer_encoded {
        content_length = None;
    }
    Ok(ResponseHead {
        status,
        content_length,
        chunked,
        connection_close,
    })
}

fn relay_chunked(reader: &mut impl BufRead, writer: &mut impl Write) -> io::Result<()> {
    loop {
        let line = read_crlf_line(reader, MAX_CHUNK_LINE)?;
        writer.write_all(&line)?;
        let size = line
            .strip_suffix(b"\r\n")
            .and_then(|line| line.split(|byte| *byte == b';').next())
            .and_then(|size| std::str::from_utf8(trim_ascii(size)).ok())
            .and_then(|size| u64::from_str_radix(size, 16).ok())
            .ok_or_else(|| invalid_http("HTTP response chunk size is invalid"))?;
        if size == 0 {
            loop {
                let trailer = read_crlf_line(reader, MAX_REQUEST_HEAD)?;
                writer.write_all(&trailer)?;
                if trailer == b"\r\n" {
                    return Ok(());
                }
            }
        }
        copy_exact(reader, writer, size)?;
        let mut ending = [0_u8; 2];
        reader.read_exact(&mut ending)?;
        if ending != *b"\r\n" {
            return Err(invalid_http("HTTP response chunk omitted its CRLF"));
        }
        writer.write_all(&ending)?;
    }
}

fn read_crlf_line(reader: &mut impl BufRead, limit: usize) -> io::Result<Vec<u8>> {
    let mut line = Vec::new();
    loop {
        let available = reader.fill_buf()?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "HTTP line ended before CRLF",
            ));
        }
        let mut consumed = 0;
        for byte in available {
            if line.len() == limit {
                return Err(invalid_http("HTTP line exceeds its size limit"));
            }
            line.push(*byte);
            consumed += 1;
            if line.ends_with(b"\r\n") {
                reader.consume(consumed);
                return Ok(line);
            }
        }
        reader.consume(consumed);
    }
}

fn copy_exact(reader: &mut impl Read, writer: &mut impl Write, length: u64) -> io::Result<()> {
    let copied = io::copy(&mut reader.take(length), writer)?;
    if copied == length {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("HTTP body ended after {copied} of {length} bytes"),
        ))
    }
}

fn invalid_http(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn splice_buffered(
    mut client_read: impl Read + Send + 'static,
    mut upstream_read: impl Read,
    mut client_write: TcpStream,
    mut upstream_write: TcpStream,
) {
    std::thread::spawn(move || {
        let _ = io::copy(&mut client_read, &mut upstream_write);
        let _ = upstream_write.shutdown(Shutdown::Write);
    });
    let _ = io::copy(&mut upstream_read, &mut client_write);
    let _ = client_write.shutdown(Shutdown::Write);
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

fn rewrite_request_head(
    head: &[u8],
    required_header: Option<&str>,
    allowed_users: &[String],
    authorization: &str,
) -> RequestAction {
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
    if let Some(required_header) = required_header {
        let mut identity_header_count = 0_usize;
        let mut identity = None;
        for header in request.headers.iter() {
            if header.name.eq_ignore_ascii_case(required_header) {
                identity_header_count += 1;
                if identity_header_count == 1 {
                    identity = Some(trim_ascii(header.value));
                }
            }
        }
        let Some(identity) = identity else {
            return RequestAction::Unauthorized;
        };
        if identity_header_count != 1
            || identity.is_empty()
            || (!allowed_users.is_empty()
                && !allowed_users
                    .iter()
                    .any(|allowed| allowed.as_bytes() == identity))
        {
            return RequestAction::Unauthorized;
        }
    }

    let mut content_length = None;
    let mut upgrade = false;
    for header in request.headers.iter() {
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
    let mut rewritten = Vec::with_capacity(head.len() + authorization.len() + 24);
    let _ = write!(rewritten, "{method} {path} HTTP/1.{version}\r\n");
    for header in request.headers.iter() {
        if header.name.eq_ignore_ascii_case("Authorization") {
            continue;
        }
        let _ = write!(rewritten, "{}: ", header.name);
        rewritten.extend_from_slice(header.value);
        rewritten.extend_from_slice(b"\r\n");
    }
    let _ = write!(rewritten, "Authorization: {authorization}\r\n\r\n");
    RequestAction::Forward {
        head: rewritten,
        content_length: content_length.unwrap_or(0),
        head_request: method.eq_ignore_ascii_case("HEAD"),
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
            allowed_users: Vec::new(),
            authorization: "Basic cmltejphYmNk".to_owned(),
        }
    }

    fn rewrite_trusted(head: &[u8]) -> RequestAction {
        let auth = auth();
        rewrite_request_head(
            head,
            Some(&auth.header_name),
            &auth.allowed_users,
            &auth.authorization,
        )
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
            head_request,
            upgrade,
        } = rewrite_trusted(
            b"POST /x HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\nAuthorization: Bearer attacker\r\nContent-Length: 4\r\n\r\n",
        ) else {
            panic!("trusted request forwards");
        };
        let head = String::from_utf8(head).expect("rewritten head");
        assert!(head.contains("Authorization: Basic cmltejphYmNk\r\n"));
        assert!(!head.contains("Bearer attacker"));
        assert_eq!(content_length, 4);
        assert!(!head_request);
        assert!(!upgrade);

        assert_eq!(
            rewrite_trusted(b"GET / HTTP/1.1\r\nHost: local\r\n\r\n"),
            RequestAction::Unauthorized
        );
        assert_eq!(
            rewrite_trusted(
                b"POST / HTTP/1.1\r\nX-Forwarded-User: alice\r\nTransfer-Encoding: chunked\r\n\r\n",
            ),
            RequestAction::Close
        );
    }

    #[test]
    fn trusted_header_enforces_the_user_allowlist_and_single_occurrence() {
        let mut allowlisted = auth();
        allowlisted.allowed_users = vec!["alice".to_owned()];

        assert!(matches!(
            rewrite_request_head(
                b"GET / HTTP/1.1\r\nX-Forwarded-User:  alice \t\r\n\r\n",
                Some(&allowlisted.header_name),
                &allowlisted.allowed_users,
                &allowlisted.authorization,
            ),
            RequestAction::Forward { .. }
        ));
        assert_eq!(
            rewrite_request_head(
                b"GET / HTTP/1.1\r\nX-Forwarded-User: Alice\r\n\r\n",
                Some(&allowlisted.header_name),
                &allowlisted.allowed_users,
                &allowlisted.authorization,
            ),
            RequestAction::Unauthorized
        );

        for auth in [auth(), allowlisted] {
            assert_eq!(
                rewrite_request_head(
                    b"GET / HTTP/1.1\r\nX-Forwarded-User: alice\r\nX-Forwarded-User: alice\r\n\r\n",
                    Some(&auth.header_name),
                    &auth.allowed_users,
                    &auth.authorization,
                ),
                RequestAction::Unauthorized
            );
        }
    }

    #[test]
    fn tunnel_relay_replaces_client_authorization_without_a_trusted_header() {
        let ignored_allowlist = ["nobody".to_owned()];
        let RequestAction::Forward { head, .. } = rewrite_request_head(
            b"GET / HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\nX-Forwarded-User: alice\r\nAuthorization: Bearer attacker\r\n\r\n",
            None,
            &ignored_allowlist,
            &auth().authorization,
        ) else {
            panic!("tunnel request forwards");
        };
        let head = String::from_utf8(head).expect("rewritten head");
        assert!(head.contains("Authorization: Basic cmltejphYmNk\r\n"));
        assert!(!head.contains("Bearer attacker"));
    }

    #[test]
    fn chunked_response_framing_is_relayed_without_rewriting() {
        let chunked = b"4\r\nWiki\r\n5;kind=test\r\npedia\r\n0\r\nTrailer: yes\r\n\r\n";
        let mut input = BufReader::new(std::io::Cursor::new(
            [chunked.as_slice(), b"next-response"].concat(),
        ));
        let mut output = Vec::new();

        relay_chunked(&mut input, &mut output).expect("relay chunked body");

        assert_eq!(output, chunked);
        let mut remaining = String::new();
        input
            .read_to_string(&mut remaining)
            .expect("read remaining response");
        assert_eq!(remaining, "next-response");
    }

    #[test]
    fn missing_trusted_header_returns_unauthorized() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, upstream) = tcp_pair();
        let gate = std::thread::spawn(move || {
            let auth = auth();
            relay_authorized(
                gate_client,
                gate_upstream,
                Some(&auth.header_name),
                &auth.allowed_users,
                &auth.authorization,
            );
        });
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
        let gate = std::thread::spawn(move || {
            let auth = auth();
            relay_authorized(
                gate_client,
                gate_upstream,
                Some(&auth.header_name),
                &auth.allowed_users,
                &auth.authorization,
            );
        });
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
    fn tunnel_relay_rewrites_each_keep_alive_request() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, mut upstream) = tcp_pair();
        let gate = std::thread::spawn(move || {
            relay_authorized(gate_client, gate_upstream, None, &[], &auth().authorization);
        });
        let upstream_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(upstream.try_clone().expect("clone upstream"));
            for path in ["/one", "/two"] {
                let head = read_request_head(&mut reader)
                    .expect("read request")
                    .expect("request head");
                let text = String::from_utf8(head).expect("request text");
                assert!(text.starts_with(&format!("GET {path} HTTP/1.1\r\n")));
                assert!(text.contains("Authorization: Basic cmltejphYmNk\r\n"));
                upstream
                    .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                    .expect("write response");
            }
        });
        let mut reader = BufReader::new(client.try_clone().expect("clone client"));
        for path in ["/one", "/two"] {
            write!(client, "GET {path} HTTP/1.1\r\nHost: local\r\n\r\n").expect("write request");
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
    fn rejected_websocket_upgrade_keeps_rewriting_requests() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, mut upstream) = tcp_pair();
        let gate = std::thread::spawn(move || {
            let auth = auth();
            relay_authorized(
                gate_client,
                gate_upstream,
                Some(&auth.header_name),
                &auth.allowed_users,
                &auth.authorization,
            );
        });
        let upstream_thread = std::thread::spawn(move || {
            let mut reader = BufReader::new(upstream.try_clone().expect("clone upstream"));
            let upgrade = read_request_head(&mut reader)
                .expect("read upgrade request")
                .expect("upgrade request");
            assert!(
                upgrade
                    .windows(b"Upgrade: websocket".len())
                    .any(|bytes| bytes == b"Upgrade: websocket")
            );
            upstream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .expect("reject upgrade");
            let second = read_request_head(&mut reader)
                .expect("read second request")
                .expect("second request");
            let second = String::from_utf8(second).expect("second request text");
            assert!(second.starts_with("GET /two HTTP/1.1\r\n"), "{second}");
            assert!(
                second.contains("Authorization: Basic cmltejphYmNk\r\n"),
                "{second}"
            );
            upstream
                .write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
                .expect("write second response");
        });
        let mut reader = BufReader::new(client.try_clone().expect("clone client"));
        client
            .write_all(b"GET /ws HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\n")
            .expect("write upgrade request");
        let rejected = read_request_head(&mut reader)
            .expect("read rejected upgrade")
            .expect("rejected upgrade response");
        assert!(rejected.starts_with(b"HTTP/1.1 200 OK"));
        client
            .write_all(b"GET /two HTTP/1.1\r\nHost: local\r\nX-Forwarded-User: alice\r\n\r\n")
            .expect("write second request");
        let response = read_request_head(&mut reader)
            .expect("read second response")
            .expect("second response");
        assert!(response.starts_with(b"HTTP/1.1 204 No Content"));
        client.shutdown(Shutdown::Both).expect("close client");
        upstream_thread.join().expect("upstream thread");
        gate.join().expect("gate thread");
    }

    #[test]
    fn safari_websocket_without_authorization_is_injected_and_spliced() {
        let (mut client, gate_client) = tcp_pair();
        let (gate_upstream, mut upstream) = tcp_pair();
        let (sent, received) = mpsc::channel();
        let gate = std::thread::spawn(move || {
            relay_authorized(gate_client, gate_upstream, None, &[], &auth().authorization);
        });
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
            .write_all(b"GET /ws HTTP/1.1\r\nHost: local\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\r\nclient-frame")
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

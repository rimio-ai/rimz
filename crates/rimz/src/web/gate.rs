//! Always-on writable gate for named tokens or trusted headers, and the loopback tunnel relay.

use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use base64::{Engine as _, engine::general_purpose::STANDARD};

use super::{Result, WebCredential, WebErr};

const MAX_REQUEST_HEAD: usize = 64 * 1024;
const MAX_HEADERS: usize = 128;
const MAX_CHUNK_LINE: usize = 8 * 1024;
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HEAD_DEADLINE: Duration = Duration::from_secs(30);
const MAX_UNAUTHENTICATED: usize = 64;
const UNAUTHORIZED: &[u8] =
    b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BASIC_UNAUTHORIZED: &[u8] = b"HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"RimZ\"\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

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

fn public_peer_allowed(peer: IpAddr, allow: &[Cidr], trusted_header: bool) -> bool {
    (!trusted_header && allow.is_empty()) || peer_allowed(peer, allow)
}

fn basic_authenticated(headers: &[httparse::Header<'_>], tokens: &[WebCredential]) -> bool {
    let mut authorization = headers
        .iter()
        .filter(|header| header.name.eq_ignore_ascii_case("Authorization"));
    let Some(header) = authorization.next() else {
        return false;
    };
    if authorization.next().is_some() {
        return false;
    }
    let Ok(value) = std::str::from_utf8(trim_ascii(header.value)) else {
        return false;
    };
    let Some((scheme, encoded)) = value.split_once(' ') else {
        return false;
    };
    if !scheme.eq_ignore_ascii_case("Basic") {
        return false;
    }
    let Ok(decoded) = STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Ok(decoded) = std::str::from_utf8(&decoded) else {
        return false;
    };
    let Some((name, secret)) = decoded.split_once(':') else {
        return false;
    };
    tokens.iter().fold(false, |matched, token| {
        matched
            | (constant_time_equal(name.as_bytes(), token.username.as_bytes())
                & constant_time_equal(secret.as_bytes(), token.secret.as_bytes()))
    })
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (index, byte) in right.iter().enumerate() {
        difference |= usize::from(left.get(index).copied().unwrap_or(0) ^ byte);
    }
    difference == 0
}

pub(super) fn serve(
    listen: SocketAddr,
    upstream: SocketAddr,
    allow: Vec<Cidr>,
    auth: Option<GateAuth>,
) -> Result<()> {
    let (credential, tokens) = super::ttyd::gate_credentials()?;
    let authorization = credential.authorization();
    let listener = TcpListener::bind(listen).map_err(|source| WebErr::GateIo {
        action: "binding its listener",
        source,
    })?;
    let unauthenticated = UnauthenticatedSlots::new(MAX_UNAUTHENTICATED);
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
        if !public_peer_allowed(peer.ip(), &allow, auth.is_some()) {
            continue;
        }
        let Some(slot) = unauthenticated.acquire() else {
            continue;
        };
        if let Some(auth) = auth.clone() {
            let _ = std::thread::Builder::new().spawn(move || {
                relay_authorized(
                    client,
                    || TcpStream::connect_timeout(&upstream, UPSTREAM_CONNECT_TIMEOUT),
                    Some(&auth.header_name),
                    &auth.allowed_users,
                    &auth.authorization,
                    None,
                    HeadLimits {
                        deadline: HEAD_DEADLINE,
                        slot: Some(slot),
                    },
                );
            });
        } else {
            let tokens = tokens.clone();
            let authorization = authorization.clone();
            let _ = std::thread::Builder::new().spawn(move || {
                relay_authorized(
                    client,
                    || TcpStream::connect_timeout(&upstream, UPSTREAM_CONNECT_TIMEOUT),
                    None,
                    &[],
                    &authorization,
                    Some(&tokens),
                    HeadLimits {
                        deadline: HEAD_DEADLINE,
                        slot: Some(slot),
                    },
                );
            });
        }
    }
}

pub(super) fn serve_tunnel(listener: TcpListener, target: Arc<Mutex<RelayTarget>>) -> Result<()> {
    let unauthenticated = UnauthenticatedSlots::new(MAX_UNAUTHENTICATED);
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
        let Some(slot) = unauthenticated.acquire() else {
            continue;
        };
        let target = target
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        let _ = std::thread::Builder::new().spawn(move || {
            relay_authorized(
                client,
                || TcpStream::connect_timeout(&target.upstream, UPSTREAM_CONNECT_TIMEOUT),
                None,
                &[],
                &target.authorization,
                None,
                HeadLimits {
                    deadline: HEAD_DEADLINE,
                    slot: Some(slot),
                },
            );
        });
    }
}

/// Bounds the connections that have not yet sent an authorized request head,
/// so peers that never authenticate cannot pin unbounded gate threads.
struct UnauthenticatedSlots(Arc<AtomicUsize>, usize);

impl UnauthenticatedSlots {
    fn new(limit: usize) -> Self {
        Self(Arc::new(AtomicUsize::new(0)), limit)
    }

    fn acquire(&self) -> Option<UnauthenticatedSlot> {
        self.0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < self.1).then_some(held + 1)
            })
            .ok()
            .map(|_| UnauthenticatedSlot(self.0.clone()))
    }
}

struct UnauthenticatedSlot(Arc<AtomicUsize>);

/// How long each request head may take, and the unauthenticated slot the
/// connection holds until its first request is authorized.
struct HeadLimits {
    deadline: Duration,
    slot: Option<UnauthenticatedSlot>,
}

impl Drop for UnauthenticatedSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
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
    mut connect_upstream: impl FnMut() -> io::Result<TcpStream>,
    required_header: Option<&str>,
    allowed_users: &[String],
    authorization: &str,
    tokens: Option<&[WebCredential]>,
    mut limits: HeadLimits,
) {
    let _ = client.set_nodelay(true);
    let Ok(client_read) = client.try_clone() else {
        return;
    };
    let mut client_read = BufReader::new(client_read);
    let mut connection = None;

    loop {
        let deadline = Instant::now() + limits.deadline;
        let action = match read_head_until(&mut client_read, || {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|remaining| !remaining.is_zero())
                .ok_or(io::ErrorKind::TimedOut)?;
            client.set_read_timeout(Some(remaining))
        }) {
            Ok(Some(head)) => {
                rewrite_request_head(&head, required_header, allowed_users, authorization, tokens)
            }
            Ok(None) => break,
            Err(_) => RequestAction::Close,
        };
        if client.set_read_timeout(None).is_err() {
            break;
        }
        let RequestAction::Forward {
            head,
            content_length,
            head_request,
            upgrade,
        } = action
        else {
            if action == RequestAction::Unauthorized {
                let _ = client.write_all(if tokens.is_some() {
                    BASIC_UNAUTHORIZED
                } else {
                    UNAUTHORIZED
                });
                let _ = client.shutdown(Shutdown::Both);
            }
            break;
        };
        drop(limits.slot.take());
        let Ok((upstream, upstream_read)) = connection.get_or_insert_with(|| {
            let upstream = connect_upstream()?;
            let _ = upstream.set_nodelay(true);
            let reader = BufReader::new(upstream.try_clone()?);
            Ok::<_, io::Error>((upstream, reader))
        }) else {
            break;
        };
        if upstream.write_all(&head).is_err()
            || copy_exact(&mut client_read, upstream, content_length).is_err()
        {
            break;
        }
        match relay_response(upstream_read, &mut client, head_request, upgrade) {
            Ok(ResponseAction::NextRequest) => {}
            Ok(ResponseAction::Upgrade) => {
                if let Some(Ok((upstream, upstream_read))) = connection.take() {
                    splice_buffered(client_read, upstream_read, client, upstream);
                }
                return;
            }
            Ok(ResponseAction::Close) | Err(_) => break,
        }
    }
    if let Some(Ok((upstream, _))) = connection {
        let _ = upstream.shutdown(Shutdown::Write);
    }
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
    read_head_until(reader, || Ok(()))
}

/// Read one head, calling `before_fill` before each buffer refill so a caller
/// can bound the whole head rather than each socket read.
fn read_head_until(
    reader: &mut impl BufRead,
    mut before_fill: impl FnMut() -> io::Result<()>,
) -> io::Result<Option<Vec<u8>>> {
    let mut head = Vec::new();
    loop {
        before_fill()?;
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
    tokens: Option<&[WebCredential]>,
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
    if tokens.is_some_and(|tokens| !basic_authenticated(request.headers, tokens)) {
        return RequestAction::Unauthorized;
    }
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

#[cfg(test)]
mod tests;

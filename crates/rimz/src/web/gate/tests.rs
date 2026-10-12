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
        None,
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
fn public_basic_admission_preserves_each_modes_reach() {
    let peer = "192.0.2.1".parse().expect("IP");
    assert!(public_peer_allowed(peer, &[], false));
    assert!(!public_peer_allowed(peer, &[], true));
    assert!(!public_peer_allowed(peer, &[parse("10.0.0.0/8")], false));
    assert!(public_peer_allowed(peer, &[parse("192.0.2.0/24")], false));
    assert!(!peer_allowed(peer, &[]), "tunnel stays loopback-only");
    assert!(
        !basic_authenticated(&[], &[]),
        "admitted peers still need a token"
    );
    assert_eq!(
        rewrite_request_head(
            b"GET / HTTP/1.1\r\nHost: local\r\n\r\n",
            None,
            &[],
            "Basic upstream",
            Some(&[])
        ),
        RequestAction::Unauthorized
    );
}

#[test]
fn basic_auth_requires_one_header_and_a_matching_named_token() {
    let tokens = [WebCredential {
        username: "alice".to_owned(),
        secret: "secret".to_owned(),
    }];
    let valid = httparse::Header {
        name: "Authorization",
        value: b"Basic YWxpY2U6c2VjcmV0",
    };
    assert!(basic_authenticated(&[valid], &tokens));
    assert!(basic_authenticated(
        &[httparse::Header {
            name: "Authorization",
            value: b"Basic   YWxpY2U6c2VjcmV0",
        }],
        &tokens
    ));
    let RequestAction::Forward { head, .. } = rewrite_request_head(
        b"GET / HTTP/1.1\r\nAuthorization: Basic YWxpY2U6c2VjcmV0\r\n\r\n",
        None,
        &[],
        "Basic upstream",
        Some(&tokens),
    ) else {
        panic!("valid token forwards");
    };
    let head = String::from_utf8(head).expect("request");
    assert!(head.contains("Authorization: Basic upstream\r\n"));
    assert!(!head.contains("YWxpY2U6c2VjcmV0"));
    assert!(!basic_authenticated(&[], &tokens));
    assert!(!basic_authenticated(&[valid, valid], &tokens));
    for value in [
        b"Basic YWxpY2U6YmFk".as_slice(),
        b"Bearer YWxpY2U6c2VjcmV0",
        b"Basic %%%",
        b"Basic Ym9iOnNlY3JldA==",
    ] {
        assert!(!basic_authenticated(
            &[httparse::Header {
                name: "authorization",
                value
            }],
            &tokens
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
            None,
        ),
        RequestAction::Forward { .. }
    ));
    assert_eq!(
        rewrite_request_head(
            b"GET / HTTP/1.1\r\nX-Forwarded-User: Alice\r\n\r\n",
            Some(&allowlisted.header_name),
            &allowlisted.allowed_users,
            &allowlisted.authorization,
            None,
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
                None,
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
    None,
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
fn idle_client_times_out_without_connecting_upstream() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mut client, gate_client) = tcp_pair();
    let connected = Arc::new(AtomicBool::new(false));
    let observed = connected.clone();
    let gate = std::thread::spawn(move || {
        relay_authorized(
            gate_client,
            || {
                connected.store(true, Ordering::SeqCst);
                let (upstream, _peer) = tcp_pair();
                Ok(upstream)
            },
            None,
            &[],
            "Basic upstream",
            Some(&[]),
            Duration::from_millis(30),
        );
    });
    client
        .set_read_timeout(Some(Duration::from_secs(1)))
        .expect("timeout");
    let result = client.read(&mut [0]);
    client.shutdown(Shutdown::Both).expect("close");
    gate.join().expect("gate");
    assert!(
        matches!(result, Ok(0)),
        "idle client must close on head timeout: {result:?}"
    );
    assert!(
        !observed.load(Ordering::SeqCst),
        "idle client must not connect upstream"
    );
}

#[test]
fn unauthorized_client_does_not_connect_upstream() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let (mut client, gate_client) = tcp_pair();
    let connected = Arc::new(AtomicBool::new(false));
    let observed = connected.clone();
    let gate = std::thread::spawn(move || {
        relay_authorized(
            gate_client,
            || {
                connected.store(true, Ordering::SeqCst);
                let (upstream, _peer) = tcp_pair();
                Ok(upstream)
            },
            None,
            &[],
            "Basic upstream",
            Some(&[]),
            Duration::from_millis(30),
        );
    });
    client
        .write_all(b"GET / HTTP/1.1\r\n\r\n")
        .expect("request");
    let mut response = String::new();
    client.read_to_string(&mut response).expect("response");
    gate.join().expect("gate");
    assert!(response.starts_with("HTTP/1.1 401"));
    assert!(
        !observed.load(Ordering::SeqCst),
        "unauthorized client must not connect upstream"
    );
}

#[test]
fn missing_trusted_header_returns_unauthorized() {
    let (mut client, gate_client) = tcp_pair();
    let (gate_upstream, upstream) = tcp_pair();
    let gate = std::thread::spawn(move || {
        let auth = auth();
        relay_authorized(
            gate_client,
            || gate_upstream.try_clone(),
            Some(&auth.header_name),
            &auth.allowed_users,
            &auth.authorization,
            None,
            Duration::from_secs(30),
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
            || gate_upstream.try_clone(),
            Some(&auth.header_name),
            &auth.allowed_users,
            &auth.authorization,
            None,
            Duration::from_secs(30),
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
        relay_authorized(
            gate_client,
            || gate_upstream.try_clone(),
            None,
            &[],
            &auth().authorization,
            None,
            Duration::from_secs(30),
        );
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
            || gate_upstream.try_clone(),
            Some(&auth.header_name),
            &auth.allowed_users,
            &auth.authorization,
            None,
            Duration::from_secs(30),
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
        relay_authorized(
            gate_client,
            || gate_upstream.try_clone(),
            None,
            &[],
            &auth().authorization,
            None,
            Duration::from_secs(30),
        );
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

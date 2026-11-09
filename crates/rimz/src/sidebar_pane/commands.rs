//! One-shot card requests on the room host's attach socket.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::MuxName;
use crate::ids::{AgentKind, AgentSessionId, PaneId};

pub(super) const COMMAND_TIMEOUT: Duration = Duration::from_secs(5);

/// Direction through the acting view's displayed cards or worktree groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardDirection {
    Next,
    Prev,
}

/// A card action resolved by the host, never by a fresh CLI snapshot.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum CardRequest {
    Walk {
        direction: CardDirection,
        worktree: bool,
    },
    Slot {
        n: u8,
    },
    Last,
    FilterNeedsYou {
        toggle: bool,
    },
    CloseTarget {
        /// The pane whose card to close; without it, the acting view's highlight.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pane: Option<PaneId>,
    },
}

impl CardRequest {
    pub(super) fn refused(&self, reason: impl std::fmt::Display) -> CardReply {
        let command = match self {
            Self::Walk { .. } => "walk",
            Self::Slot { .. } => "slot",
            Self::Last => "last",
            Self::FilterNeedsYou { .. } => "filter needs-you",
            Self::CloseTarget { .. } => "close",
        };
        CardReply::Refused {
            reason: format!("sidebar {command}: {reason}"),
        }
    }
}

/// The highlight's pane and optional session key, for the CLI's close path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardTarget {
    Agent {
        kind: AgentKind,
        agent_id: AgentSessionId,
        pane: PaneId,
    },
    Pane(PaneId),
}

/// A completed reduction, a no-op reason, or the highlighted close target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reply", rename_all = "snake_case")]
pub enum CardReply {
    Done,
    Refused {
        reason: String,
    },
    CloseTarget {
        #[serde(flatten)]
        target: CardTarget,
    },
}

/// Ask the session's sidebar host to act, with a bounded reply wait.
pub fn request_card(mux: MuxName, session: &str, request: CardRequest) -> io::Result<CardReply> {
    let record = crate::room::session::workspace_record_for_session(session)
        .map_err(io::Error::other)?
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no room for session {session}"),
            )
        })?;
    let runtime =
        crate::RuntimePaths::for_project_root(&record.project_root).map_err(io::Error::other)?;
    request_at(
        &runtime.sidebar_host_socket_path(mux, session),
        request,
        COMMAND_TIMEOUT,
    )
}

fn request_at(path: &Path, request: CardRequest, timeout: Duration) -> io::Result<CardReply> {
    exchange(path, request, Instant::now() + timeout).map_err(|error| {
        let kind = match error.kind() {
            io::ErrorKind::WouldBlock => io::ErrorKind::TimedOut,
            kind => kind,
        };
        io::Error::new(
            kind,
            format!("sidebar host: {error}; run `rimz sidebar repair`"),
        )
    })
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|remaining| !remaining.is_zero())
        .ok_or_else(|| io::ErrorKind::TimedOut.into())
}

fn exchange(path: &Path, request: CardRequest, deadline: Instant) -> io::Result<CardReply> {
    let mut stream = UnixStream::connect(path)?;
    let mut request = serde_json::to_vec(&request).map_err(io::Error::other)?;
    request.push(b'\n');
    let mut pending = request.as_slice();
    while !pending.is_empty() {
        stream.set_write_timeout(Some(remaining(deadline)?))?;
        match stream.write(pending) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(written) => pending = &pending[written..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    let mut reply = Vec::new();
    while !reply.ends_with(b"\n") {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(read) => reply.extend_from_slice(&chunk[..read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
        if reply.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "reply exceeds its limit",
            ));
        }
    }
    serde_json::from_slice(&reply)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    #[test]
    fn close_keeps_its_bare_wire_and_carries_an_optional_pane() {
        let bare = serde_json::json!({"command": "close_target"});
        assert_eq!(
            serde_json::to_value(CardRequest::CloseTarget { pane: None }).unwrap(),
            bare
        );
        assert_eq!(
            serde_json::from_value::<CardRequest>(bare).unwrap(),
            CardRequest::CloseTarget { pane: None }
        );
        let pane = PaneId::from_parts(MuxName::Tmux, "%7");
        let scoped = CardRequest::CloseTarget {
            pane: Some(pane.clone()),
        };
        let wire = serde_json::to_value(&scoped).unwrap();
        assert_eq!(wire["pane"], serde_json::json!(pane));
        assert_eq!(serde_json::from_value::<CardRequest>(wire).unwrap(), scoped);
    }

    #[test]
    fn card_client_exchanges_one_tagged_request_and_reply() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            let mut line = String::new();
            BufReader::new(peer.try_clone().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).unwrap(),
                serde_json::json!({"command": "walk", "direction": "next", "worktree": false})
            );
            peer.write_all(b"{\"reply\":\"done\"}\n").unwrap();
        });
        let reply = request_at(
            &path,
            CardRequest::Walk {
                direction: CardDirection::Next,
                worktree: false,
            },
            COMMAND_TIMEOUT,
        );
        assert!(matches!(reply, Ok(CardReply::Done)), "{reply:?}");
        server.join().unwrap();
    }

    #[test]
    fn card_client_missing_host_and_eof_name_repair() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("host.sock");
        let error = request_at(&path, CardRequest::Last, COMMAND_TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("rimz sidebar repair"), "{error}");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || drop(listener.accept().unwrap()));
        let error = request_at(&path, CardRequest::Last, COMMAND_TIMEOUT).unwrap_err();
        assert!(error.to_string().contains("rimz sidebar repair"), "{error}");
        server.join().unwrap();
    }

    #[test]
    fn card_client_deadline_does_not_restart_on_partial_reply() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let path = dir.path().join("host.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            let (mut peer, _) = listener.accept().unwrap();
            for _ in 0..100 {
                if peer.write_all(b" ").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let started = Instant::now();
        let error = request_at(&path, CardRequest::Last, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut, "{error}");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "partial reads extended the budget"
        );
        assert!(error.to_string().contains("rimz sidebar repair"), "{error}");
        server.join().unwrap();
    }
}

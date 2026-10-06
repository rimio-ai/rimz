//! Wire of the pane-to-host attach: a supervisor hands its pane's output to the room host over a stream socket and then only listens.
//!
//! One connection is one pane. The supervisor sends a hello line with the pane's output fd riding the same message, the host answers accept or reject, and from there the host alone writes, one control line at a time. End of stream from either side detaches the pane. The contract lives in `docs/internals/sidebar/state.md`.

use std::io::{self, BufRead, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags, recvmsg, sendmsg,
};
use serde::{Deserialize, Serialize};

use crate::ids::{PaneId, SidebarInstanceId};

pub(super) const PROTOCOL: &str = "rimz.sidebar-attach.v1";
/// A hello is one short JSON line; anything longer is not this protocol.
const HELLO_LIMIT: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct Hello {
    pub(super) protocol: String,
    pub(super) instance_id: SidebarInstanceId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) pane_id: Option<PaneId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) supervisor_build: Option<String>,
    /// The pane's launch-argv cadence overrides, which the host's own argv
    /// cannot carry for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) tick_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) refresh_ms: Option<u16>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Reply {
    Accept {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        build: Option<String>,
    },
    Reject {
        reason: String,
    },
}

/// Reject reasons a supervisor can act on by name; any other text is a
/// host-side failure it only logs.
pub(super) const REJECT_PROTOCOL: &str = "protocol";
pub(super) const REJECT_BUILD: &str = "build";
pub(super) const REJECT_CAPACITY: &str = "capacity";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct ControlLine {
    pub(super) control: Control,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Control {
    /// The pane's tab looks empty: confirm against the mux and close.
    SelfClose,
    /// The host is leaving for a newer build: attach again.
    Reload,
}

/// Send the hello and the pane's output fd as one message.
pub(super) fn send_hello(
    stream: &UnixStream,
    hello: &Hello,
    output: BorrowedFd<'_>,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(hello).map_err(io::Error::other)?;
    line.push(b'\n');
    if line.len() > HELLO_LIMIT {
        return Err(io::Error::other("sidebar attach hello exceeds its limit"));
    }
    let fds = [output];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&fds)) {
        return Err(io::Error::other("no room for the pane fd"));
    }
    #[cfg(target_os = "macos")]
    rustix::net::sockopt::set_socket_nosigpipe(stream, true)?;
    #[cfg(target_os = "macos")]
    let flags = SendFlags::empty();
    #[cfg(not(target_os = "macos"))]
    let flags = SendFlags::NOSIGNAL;
    let sent = sendmsg(stream, &[IoSlice::new(&line)], &mut ancillary, flags)?;
    if sent != line.len() {
        return Err(io::Error::other("sidebar attach hello was cut short"));
    }
    Ok(())
}

/// Read the hello and the fd that rode with it. `None` is a peer that closed
/// without saying anything, such as a liveness probe of the socket.
pub(super) fn recv_hello(stream: &UnixStream) -> io::Result<Option<(Hello, OwnedFd)>> {
    #[cfg(target_os = "macos")]
    let flags = RecvFlags::empty();
    #[cfg(not(target_os = "macos"))]
    let flags = RecvFlags::CMSG_CLOEXEC;
    let mut line = Vec::new();
    let mut output = None;
    while !line.ends_with(b"\n") {
        if line.len() >= HELLO_LIMIT {
            return Err(io::Error::other("sidebar attach hello exceeds its limit"));
        }
        let mut chunk = [0u8; HELLO_LIMIT];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let received = recvmsg(
            stream,
            &mut [IoSliceMut::new(&mut chunk)],
            &mut ancillary,
            flags,
        )?;
        for message in ancillary.drain() {
            if let RecvAncillaryMessage::ScmRights(fds) = message {
                for fd in fds {
                    // macOS has no atomic MSG_CMSG_CLOEXEC; mark each received
                    // fd immediately, before keeping it or spawning more work.
                    #[cfg(target_os = "macos")]
                    rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
                    // Keep the first fd; any other closes as it drops.
                    if output.is_none() {
                        output = Some(fd);
                    }
                }
            }
        }
        if received.bytes == 0 {
            return match line.is_empty() {
                true => Ok(None),
                false => Err(io::ErrorKind::UnexpectedEof.into()),
            };
        }
        line.extend_from_slice(&chunk[..received.bytes]);
    }
    let hello = serde_json::from_slice(&line).map_err(io::Error::other)?;
    let output = output.ok_or_else(|| io::Error::other("sidebar attach hello carried no fd"))?;
    Ok(Some((hello, output)))
}

pub(super) fn write_line<T: Serialize>(mut stream: &UnixStream, line: &T) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(line).map_err(io::Error::other)?;
    bytes.push(b'\n');
    stream.write_all(&bytes)
}

/// The next line from the host, or `None` at end of stream.
pub(super) fn read_line<T: for<'de> Deserialize<'de>>(
    reader: &mut impl BufRead,
) -> io::Result<Option<T>> {
    let mut line = String::new();
    if reader.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    serde_json::from_str(&line)
        .map(Some)
        .map_err(io::Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::BufReader;
    use std::os::fd::AsFd;

    fn hello() -> Hello {
        Hello {
            protocol: PROTOCOL.to_owned(),
            instance_id: SidebarInstanceId::new(),
            pane_id: Some(PaneId::from_parts(crate::MuxName::Tmux, "%7")),
            supervisor_build: Some("build-a".to_owned()),
            tick_seconds: Some(2),
            refresh_ms: Some(250),
        }
    }

    #[test]
    fn a_hello_carries_only_what_the_host_reads() {
        let line = serde_json::to_value(hello()).unwrap();
        let fields: std::collections::BTreeSet<&str> = line
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(
            fields,
            std::collections::BTreeSet::from([
                "instance_id",
                "pane_id",
                "protocol",
                "refresh_ms",
                "supervisor_build",
                "tick_seconds",
            ])
        );
    }

    #[test]
    fn the_hello_arrives_with_the_fd_that_rode_it() {
        let (supervisor, host) = UnixStream::pair().unwrap();
        let (pane, mut far_end) = UnixStream::pair().unwrap();
        let sent = hello();
        send_hello(&supervisor, &sent, pane.as_fd()).unwrap();

        let (received, output) = recv_hello(&host).unwrap().expect("a hello");
        assert_eq!(received, sent);
        assert!(
            rustix::io::fcntl_getfd(&output)
                .unwrap()
                .contains(rustix::io::FdFlags::CLOEXEC)
        );
        drop(pane);
        std::fs::File::from(output).write_all(b"frame").unwrap();
        let mut frame = [0u8; 5];
        std::io::Read::read_exact(&mut far_end, &mut frame).unwrap();
        assert_eq!(&frame, b"frame", "the received fd is the pane's output");
    }

    #[test]
    fn a_closed_host_returns_an_error_when_sending_a_hello() {
        let (supervisor, host) = UnixStream::pair().unwrap();
        let (pane, _far_end) = UnixStream::pair().unwrap();
        drop(host);
        assert_eq!(
            send_hello(&supervisor, &hello(), pane.as_fd())
                .unwrap_err()
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[test]
    fn a_peer_that_closes_silently_is_no_hello_and_no_error() {
        let (probe, host) = UnixStream::pair().unwrap();
        drop(probe);
        assert!(recv_hello(&host).unwrap().is_none());
    }

    #[test]
    fn a_hello_without_an_fd_is_refused() {
        let (supervisor, host) = UnixStream::pair().unwrap();
        write_line(&supervisor, &hello()).unwrap();
        assert!(recv_hello(&host).is_err());
    }

    #[test]
    fn replies_and_controls_keep_their_wire_spelling() {
        let wire = |line: &dyn erased::Line| line.json();
        assert_eq!(
            wire(&Reply::Accept {
                build: Some("b".to_owned())
            }),
            r#"{"accept":{"build":"b"}}"#
        );
        assert_eq!(
            wire(&Reply::Reject {
                reason: REJECT_PROTOCOL.to_owned()
            }),
            r#"{"reject":{"reason":"protocol"}}"#
        );
        assert_eq!(
            wire(&ControlLine {
                control: Control::SelfClose
            }),
            r#"{"control":"self-close"}"#
        );
        assert_eq!(
            wire(&ControlLine {
                control: Control::Reload
            }),
            r#"{"control":"reload"}"#
        );
    }

    #[test]
    fn control_lines_read_back_until_end_of_stream() {
        let (host, supervisor) = UnixStream::pair().unwrap();
        write_line(
            &host,
            &ControlLine {
                control: Control::Reload,
            },
        )
        .unwrap();
        drop(host);
        let mut reader = BufReader::new(supervisor);
        assert_eq!(
            read_line::<ControlLine>(&mut reader).unwrap(),
            Some(ControlLine {
                control: Control::Reload
            })
        );
        assert_eq!(read_line::<ControlLine>(&mut reader).unwrap(), None);
    }

    mod erased {
        pub(super) trait Line {
            fn json(&self) -> String;
        }

        impl<T: serde::Serialize> Line for T {
            fn json(&self) -> String {
                serde_json::to_string(self).unwrap()
            }
        }
    }
}

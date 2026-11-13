//! `rimz tab dashboard` and `rimz tab terminal N` against a started room,
//! shared by both backends.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use rimz::ids::PaneId;
use rimz::mux::{ClientFocusOptions, MuxBackend, PaneListOptions, PaneReadConsistency};
use rimz::pane::PaneRef;

use crate::common::{CommandTimeoutExt, Env};

/// The live room the checks drive: its backend, session, and a way to run
/// `rimz` against it.
pub(super) struct TabRoom<'a, C: Fn() -> Command> {
    pub(super) env: &'a Env,
    pub(super) backend: &'a dyn MuxBackend,
    pub(super) session: &'a str,
    pub(super) runtime_root: &'a Path,
    pub(super) command: C,
}

impl<C: Fn() -> Command> TabRoom<'_, C> {
    /// Run `rimz tab ARGS --session-name S` from a directory outside the room.
    pub(super) fn tab(&self, args: &[&str]) -> Output {
        (self.command)()
            .current_dir(&self.env.home_root)
            .env("SHELL", "/bin/sh")
            .arg("tab")
            .args(args)
            .args(["--session-name", self.session])
            .bounded_output()
            .unwrap()
    }

    fn shown(&self, args: &[&str]) {
        let output = self.tab(args);
        assert!(
            output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
            "{args:?}: {output:?}"
        );
    }

    fn runtime(&self) -> rimz::RuntimePaths {
        rimz::RuntimePaths::for_state_under(
            &self.env.state_path_for(&self.env.project_root),
            self.runtime_root,
        )
    }

    pub(super) fn panes(&self) -> Vec<PaneRef> {
        self.backend
            .list_panes(PaneListOptions {
                session_name: Some(self.session.into()),
                runtime_paths: Some(self.runtime()),
                consistency: PaneReadConsistency::RequireAuthoritative,
                ..Default::default()
            })
            .unwrap()
            .panes
    }

    /// The ids of the views whose name, status suffix stripped, is `name`.
    pub(super) fn views_named(&self, name: &str) -> BTreeSet<String> {
        let theme = rimz::config::ThemeConfig::default();
        self.panes()
            .into_iter()
            .filter(|pane| {
                pane.view_name.as_deref().is_some_and(|view| {
                    rimz::theme::strip_status_glyph_suffix(view, &theme) == name
                })
            })
            .filter_map(|pane| pane.view_id)
            .collect()
    }

    fn viewed(&self) -> Option<PaneRef> {
        let view = self
            .backend
            .client_view(ClientFocusOptions {
                session_name: Some(self.session.into()),
                ..Default::default()
            })
            .ok()?;
        let [client] = view.clients.as_slice() else {
            return None;
        };
        self.panes()
            .into_iter()
            .find(|pane| pane.pane_id == client.pane_id)
    }

    /// Wait until the one client views a tab named `name`, the only one of
    /// that name, and return the pane it views there.
    #[track_caller]
    fn wait_viewing(&self, name: &str) -> PaneRef {
        let theme = rimz::config::ThemeConfig::default();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let viewed = self.viewed();
            if let Some(pane) = viewed.as_ref().filter(|pane| {
                pane.view_name.as_deref().is_some_and(|view| {
                    rimz::theme::strip_status_glyph_suffix(view, &theme) == name
                })
            }) {
                assert_eq!(self.views_named(name).len(), 1, "one {name}");
                return pane.clone();
            }
            assert!(
                Instant::now() < deadline,
                "client never viewed {name}: {viewed:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[track_caller]
    fn wait_gone(&self, name: &str) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while !self.views_named(name).is_empty() {
            assert!(Instant::now() < deadline, "{name} never closed");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Wait until `pane`, the focused one, is a shell reporting the project
    /// root as its cwd, beside a docked sidebar in its view.
    #[track_caller]
    fn wait_terminal_shape(&self, pane: &PaneId) {
        let root = std::fs::canonicalize(&self.env.project_root).unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let panes = self.panes();
            let shell = panes.iter().find(|candidate| candidate.pane_id == *pane);
            let siblings = shell.map_or(0, |shell| {
                panes
                    .iter()
                    .filter(|candidate| candidate.view_id == shell.view_id)
                    .count()
            });
            let is_shell = shell.is_some_and(|shell| !shell.is_rimz_sidebar());
            if cwd_of(shell) == Some(root.clone()) && is_shell && siblings >= 2 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "terminal {pane}: {shell:?}, {siblings} panes in its view"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Move the shell in `pane` to `dir`, so the next terminal opens from a
    /// focused pane outside the project root.
    #[track_caller]
    fn cd(&self, pane: &PaneId, dir: &Path) {
        let dir = std::fs::canonicalize(dir).unwrap();
        self.backend
            .send_keys(pane, self.session, &format!("cd {}", dir.display()))
            .unwrap();
        self.backend
            .send_key(pane, self.session, rimz::pane::keys::NamedKey::Enter)
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let panes = self.panes();
            let shell = panes.iter().find(|candidate| candidate.pane_id == *pane);
            if cwd_of(shell) == Some(dir.clone()) {
                return;
            }
            assert!(Instant::now() < deadline, "{pane} never moved: {shell:?}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The outcome for a session that is not a live room: each verb exits 1
    /// naming the session, whatever the backend.
    #[track_caller]
    pub(super) fn check_dead_session(&self) {
        for args in [&["terminal", "3"][..], &["dashboard"][..]] {
            let output = self.tab(args);
            assert!(
                output.status.code() == Some(1)
                    && output.stdout.is_empty()
                    && String::from_utf8_lossy(&output.stderr).contains(self.session),
                "{args:?}: {output:?}"
            );
        }
    }
}

/// Rules 1, 2, 3, 4, 5 and 6 against a room with one attached client.
/// `close_view` closes the whole view of a name, as a user closing a tab.
pub(super) fn check_tabs<C: Fn() -> Command>(room: &TabRoom<'_, C>, close_view: impl Fn(&str)) {
    room.wait_viewing_any();
    room.shown(&["terminal", "3"]);
    let term3 = room.wait_viewing("term-3").pane_id;
    room.wait_terminal_shape(&term3);
    room.cd(&term3, &room.env.home_root);
    room.shown(&["terminal", "5"]);
    let term5 = room.wait_viewing("term-5").pane_id;
    room.wait_terminal_shape(&term5);

    room.shown(&["dashboard"]);
    room.wait_viewing("rimzd");
    room.shown(&["terminal", "3"]);
    assert_eq!(room.wait_viewing("term-3").pane_id, term3);

    room.backend
        .send_keys(&term3, room.session, "sleep 347")
        .unwrap();
    room.backend
        .send_key(&term3, room.session, rimz::pane::keys::NamedKey::Enter)
        .unwrap();
    room.shown(&["terminal", "5"]);
    assert_eq!(room.wait_viewing("term-5").pane_id, term5);
    room.shown(&["terminal", "3"]);
    assert_eq!(room.wait_viewing("term-3").pane_id, term3);

    close_view("term-3");
    room.wait_gone("term-3");
    room.shown(&["terminal", "3"]);
    let reborn = room.wait_viewing("term-3").pane_id;
    assert_ne!(reborn, term3, "a closed terminal is made again");
    room.wait_terminal_shape(&reborn);
    room.shown(&["terminal", "5"]);
    assert_eq!(room.wait_viewing("term-5").pane_id, term5);

    let racers = (0..4)
        .map(|_| {
            (room.command)()
                .current_dir(&room.env.home_root)
                .env("SHELL", "/bin/sh")
                .args(["tab", "terminal", "7", "--session-name", room.session])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for racer in racers {
        let output = racer.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    room.wait_viewing("term-7");
    assert_eq!(
        room.views_named("term-7").len(),
        1,
        "one term-7 after a race"
    );

    close_view("rimzd");
    room.wait_gone("rimzd");
    room.shown(&["dashboard"]);
    room.wait_viewing("rimzd");
    assert_eq!(room.views_named("term-5").len(), 1);
    assert_eq!(room.views_named("term-3").len(), 1);
}

impl<C: Fn() -> Command> TabRoom<'_, C> {
    fn wait_viewing_any(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        while self.viewed().is_none() {
            assert!(Instant::now() < deadline, "no attached client view");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

fn cwd_of(pane: Option<&PaneRef>) -> Option<std::path::PathBuf> {
    pane.and_then(|pane| pane.cwd.as_deref())
        .and_then(|cwd| std::fs::canonicalize(cwd).ok())
}

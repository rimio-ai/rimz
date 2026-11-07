//! Status modes, the cached compact tab strip, and its refresh hooks.

use super::{ScriptOutput, TmuxBackend, parse_version};
use crate::config::MuxStatusBar;
use crate::mux::{MuxErr, Result};

const TAB_RADIUS: usize = 10;
const TAB_SPAN: usize = 2 * TAB_RADIUS;
const COMPACT_GATE: &str = "#{==:#{@rimz_status},compact}";
const COMPACT_LAYOUT: &str = concat!(
    "#[align=left range=left #{E:status-left-style}]#[push-default]",
    "#{T;=/#{status-left-length}:status-left}#[pop-default]#[norange default]",
    "#[list=on align=#{status-justify}]#[list=left-marker]<#[list=right-marker]>#[list=on]",
    "#{@rimz_tab_strip}",
    "#[nolist align=right range=right #{E:status-right-style}]#[push-default]",
    "#{T;=/#{status-right-length}:status-right}#[pop-default]#[norange default]",
);

fn strip_start_format() -> String {
    let first = "#{window_index}";
    let preferred = format!("#{{e|-:#{{active_window_index}},{TAB_RADIUS}}}");
    let last_start = format!("#{{e|-:#{{last_window_index}},{TAB_SPAN}}}");
    let inward = format!("#{{?#{{e|<:{preferred},{last_start}}},{preferred},{last_start}}}");
    format!("#{{?#{{e|<:{inward},{first}}},{first},{inward}}}")
}

fn strip_format() -> String {
    let within = format!(
        "#{{&&:#{{e|>=:#{{window_index}},#{{@rimz_tab_start}}}},\
         #{{e|<=:#{{window_index}},#{{e|+:#{{@rimz_tab_start}},{TAB_SPAN}}}}}}}"
    );
    let flags = concat!(
        "#{?#{&&:#{window_last_flag},#{!=:#{E:window-status-last-style},default}},",
        " #{E:window-status-last-style},}",
        "#{?#{&&:#{window_bell_flag},#{!=:#{E:window-status-bell-style},default}},",
        " #{E:window-status-bell-style},",
        "#{?#{&&:#{||:#{window_activity_flag},#{window_silence_flag}},",
        "#{!=:#{E:window-status-activity-style},default}}, #{E:window-status-activity-style},}}",
    );
    let separator = "#{?window_end_flag,,#{E:window-status-separator}}";
    let normal = format!(
        "#[range=window|#{{window_index}} #{{E:window-status-style}}{flags}]#[push-default]\
         #{{T:window-status-format}}#[pop-default]#[norange default]{separator}"
    );
    let current = format!(
        "#[range=window|#{{window_index}} list=focus \
         #{{?#{{!=:#{{E:window-status-current-style}},default}},\
         #{{E:window-status-current-style}},#{{E:window-status-style}}}}{flags}]#[push-default]\
         #{{T:window-status-current-format}}#[pop-default]#[norange list=on default]{separator}"
    );
    format!("#{{W:#{{?{within},{normal},}},#{{?{within},{current},}}}}")
}

fn refresh_commands(session: Option<&str>) -> Vec<Vec<String>> {
    [
        (
            "@rimz_tab_start",
            strip_start_format(),
            Some(format!("{}:^", session.unwrap_or_default())),
        ),
        (
            "@rimz_tab_strip",
            strip_format(),
            session.map(str::to_owned),
        ),
    ]
    .into_iter()
    .map(|(key, value, target)| {
        let mut command = vec!["set-option".into(), "-F".into()];
        if let Some(target) = target {
            command.extend(["-t".into(), target]);
        }
        command.extend([key.into(), value]);
        command
    })
    .collect()
}

fn quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\\''"))
}

fn command_text(commands: &[Vec<String>]) -> String {
    commands
        .iter()
        .map(|command| {
            command
                .iter()
                .map(|word| quote(word))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join(" ; ")
}

fn refresh_body(session: Option<&str>) -> String {
    command_text(&refresh_commands(session))
}

/// `compact` and `off` take `status` and `status-format` over, so they clear
/// both. `on` clears them only while the session carries the mode marker:
/// without it they hold the user's own values (a typed `set status off`).
fn status_commands(
    session: &str,
    mode: MuxStatusBar,
    global_status: &str,
    formats: &[Option<String>; 4],
    after_swap: bool,
) -> Vec<Vec<String>> {
    let unset = |key: &str| {
        ["set-option", "-qu", "-t", session, key]
            .map(str::to_owned)
            .to_vec()
    };
    let bar = [unset("status"), unset("status-format")];
    let mut commands = if mode == MuxStatusBar::On {
        vec![vec![
            "if-shell".into(),
            "-F".into(),
            "-t".into(),
            session.into(),
            "#{@rimz_status}".into(),
            command_text(&bar),
        ]]
    } else {
        bar.to_vec()
    };
    commands.extend(["@rimz_status", "@rimz_tab_strip", "@rimz_tab_start"].map(unset));
    let set = |key: &str, value: &str| {
        ["set-option", "-t", session, key, value]
            .map(str::to_owned)
            .to_vec()
    };
    match mode {
        MuxStatusBar::On => return commands,
        MuxStatusBar::Off => {
            commands.push(set("@rimz_status", "off"));
            commands.push(set("status", "off"));
            return commands;
        }
        MuxStatusBar::Compact => {}
    }
    commands.push(set("@rimz_status", "compact"));
    commands.push(set("status-format[0]", COMPACT_LAYOUT));
    for (index, value) in formats.iter().enumerate() {
        let Some(value) = value else { continue };
        commands.push(set(&format!("status-format[{}]", index + 1), value));
    }
    let hook = format!(
        "if-shell -F '{COMPACT_GATE}' {}",
        quote(&refresh_body(None))
    );
    for event in [
        "session-window-changed",
        "window-linked",
        "window-unlinked",
        "window-renamed",
        "alert-activity",
        "alert-bell",
        "alert-silence",
    ]
    .into_iter()
    .chain(after_swap.then_some("after-swap-window"))
    {
        commands.push(vec![
            "set-hook".into(),
            "-g".into(),
            format!("{event}[241]"),
            hook.clone(),
        ]);
    }
    commands.extend(refresh_commands(Some(session)));
    if global_status == "off" {
        commands.push(set("status", "on"));
    }
    commands
}

pub(super) fn swap_window_commands(session: &str, name: &str, base: &str) -> Vec<Vec<String>> {
    vec![
        vec![
            "swap-window".into(),
            "-d".into(),
            "-s".into(),
            format!("{session}:{name}"),
            "-t".into(),
            format!("{session}:{base}"),
        ],
        vec![
            "if-shell".into(),
            "-F".into(),
            "-t".into(),
            session.into(),
            COMPACT_GATE.into(),
            refresh_body(Some(session)),
        ],
    ]
}

impl TmuxBackend {
    fn status_script(&self, commands: Vec<Vec<String>>) -> Result<ScriptOutput> {
        let lines: Vec<_> = commands.into_iter().map(|command| vec![command]).collect();
        let output = self.run_script(&lines)?;
        if !output.stderr.is_empty() || output.lines.iter().any(|line| !line.completed) {
            return Err(MuxErr::Output {
                program: "tmux".into(),
                reason: if output.stderr.is_empty() {
                    "status script did not finish every line".into()
                } else {
                    output.stderr
                },
            });
        }
        Ok(output)
    }

    fn try_apply_status(&self, session: &str, mode: MuxStatusBar) -> Result<()> {
        let empty = std::array::from_fn(|_| None);
        if mode != MuxStatusBar::Compact {
            return self
                .status_script(status_commands(session, mode, "", &empty, false))
                .map(|_| ());
        }
        let mut reads: Vec<_> = [
            "status",
            "status-format[1]",
            "status-format[2]",
            "status-format[3]",
            "status-format[4]",
        ]
        .map(|key| ["show-options", "-gqv", key].map(str::to_owned).to_vec())
        .into();
        reads.push(
            ["display-message", "-p", "-t", session, "#{version}"]
                .map(str::to_owned)
                .to_vec(),
        );
        let output = self.status_script(reads)?;
        let global_status = output.lines[0].stdout.trim();
        let formats = std::array::from_fn(|index| {
            output.lines[index + 1]
                .stdout
                .strip_suffix('\n')
                .map(str::to_owned)
        });
        let after_swap =
            parse_version(&output.lines[5].stdout).is_some_and(|version| version >= (3, 8, 0));
        self.status_script(status_commands(
            session,
            mode,
            global_status,
            &formats,
            after_swap,
        ))
        .map(|_| ())
    }

    pub(super) fn apply_status(&self, session: &str, mode: MuxStatusBar) {
        let Err(err) = self.try_apply_status(session, mode) else {
            return;
        };
        tracing::warn!(
            session,
            error = &err as &dyn std::error::Error,
            "could not apply tmux status mode; restoring the inherited bar",
        );
        if let Err(err) = self.try_apply_status(session, MuxStatusBar::On) {
            tracing::warn!(
                session,
                error = &err as &dyn std::error::Error,
                "could not restore inherited tmux status options",
            );
        }
    }
}

#[cfg(test)]
mod tests;

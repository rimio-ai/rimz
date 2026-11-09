use std::io::{IsTerminal, Write};

use anyhow::Result;
use clap::Args;
use ratatui::crossterm::event::{self, Event};
use rimz::config::MachineConfig;
use rimz::config::keys::RoomKeymap;
use rimz::tui::{MouseCapture, Screen, TerminalModeGuard};

use super::{GlobalFlags, render};

#[derive(Debug, Args)]
pub(super) struct KeysArgs {
    /// Keep the key list open until a keypress.
    #[arg(long)]
    wait: bool,
}

pub(super) fn run(args: KeysArgs, _globals: &GlobalFlags) -> Result<()> {
    anyhow::ensure!(
        !args.wait || std::io::stdin().is_terminal(),
        "keys --wait needs a terminal"
    );
    let config = MachineConfig::load_lenient();
    super::report_unknown_config_keys(&config)?;
    let mut out = render::out();
    write_keymap(&mut out, &config.room_keymap())?;
    out.flush()?;
    if args.wait {
        let _terminal = TerminalModeGuard::enable(MouseCapture::Off, Screen::Main)?;
        loop {
            if matches!(event::read()?, Event::Key(_)) {
                break;
            }
        }
    }
    Ok(())
}

fn write_keymap(out: &mut impl Write, map: &RoomKeymap) -> std::io::Result<()> {
    let listed = map
        .modes
        .iter()
        .filter(|(_, bindings)| !bindings.is_empty());
    for (index, (mode, bindings)) in listed.enumerate() {
        if index > 0 {
            writeln!(out)?;
        }
        writeln!(out, "{mode}")?;
        let width = bindings
            .keys()
            .map(|key| key.to_string().len())
            .max()
            .unwrap_or(0);
        for (key, action) in bindings {
            writeln!(out, "  {:width$}  {action}", key.to_string())?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_list_shows_resolved_modes_overrides_and_quoted_run_arguments() {
        let machine: rimz::config::MachineConfig = toml::from_str(
            "[room]\nstyle='rimz'\n[keys]\nprefix='Ctrl+a'\n\
             [keys.bind.normal]\n'Alt+x'={run='rimz message @coder \"two words\"'}\n'Alt+t'='none'\n",
        )
        .unwrap();
        let mut bytes = Vec::new();
        write_keymap(&mut bytes, &machine.room_keymap()).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        let modes: Vec<_> = text
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with(' '))
            .collect();
        assert_eq!(
            modes,
            ["normal", "prefix", "resize", "move"],
            "modes list in order and an empty mode is left out: {text}"
        );
        assert!(text.contains("rimz message @coder 'two words'"), "{text}");
        assert_eq!(text.matches("Ctrl+a").count(), 2, "{text}");
        assert!(!text.contains("Alt+p"), "{text}");
        assert!(!text.contains("Alt+t"), "{text}");
    }

    #[test]
    fn config_notices_show_each_ignored_binding() {
        let mut machine = rimz::config::MachineConfig::default();
        let message = "keys.bind.normal.\"x\": expected a modifier; binding ignored";
        machine
            .notices
            .key_bindings
            .insert("keys.bind.normal.\"x\"".into(), message.into());
        let mut bytes = Vec::new();
        super::super::write_config_notices(&machine, &mut bytes).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert_eq!(text.matches(message).count(), 1, "{text}");
    }
}

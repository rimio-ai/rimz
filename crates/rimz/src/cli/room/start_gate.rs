//! The remote-control start gate: probe each host for this start, and at an
//! attended birth offer to run a blocked host off for this run or for good.

use std::collections::BTreeSet;
use std::io::{self, BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use rimz::config::MachineConfig;
use rimz::ids::AgentKind;
use rimz::remote_control::{HostLoginEnvs, ReadinessSnapshot, StartBlock};

use crate::cli::render;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Choice {
    RunOnly,
    ForGood,
    Quit,
}

const ANSWERS: [(&str, Choice); 3] = [
    ("t", Choice::RunOnly),
    ("p", Choice::ForGood),
    ("q", Choice::Quit),
];

/// Where a start meets its remote-control hosts.
pub(super) struct Gate<'a> {
    pub envs: &'a HostLoginEnvs,
    pub project_root: Option<&'a Path>,
    /// The room is already running, so this start cannot birth it again and
    /// honours the waivers its record holds; a birth judges every host afresh.
    pub live: bool,
    /// An attended start that births the room may offer a run-only choice.
    pub offers_choice: bool,
}

impl Gate<'_> {
    /// Probe under the hosts waived so far; each `[t]` or `[p]` re-probes, so a
    /// second blocked host gets its own prompt. Returns the readiness the room
    /// is born with.
    pub(super) fn run(
        &self,
        machine_config: &mut Arc<MachineConfig>,
        waived: &mut BTreeSet<AgentKind>,
    ) -> Result<ReadinessSnapshot> {
        loop {
            let machine = &machine_config.remote_control;
            let recorded = self
                .project_root
                .filter(|_| self.live)
                .and_then(|root| rimz::StatePaths::for_project_root(root).ok())
                .map_or_else(
                    || machine.clone(),
                    |state| rimz::remote_control::room_config_at(machine, &state.workspace_record),
                );
            let remote_control = recorded.without(waived.iter().map(AgentKind::as_str));
            rimz::remote_control::prepare_hosts(&remote_control, self.envs);
            let readiness = ReadinessSnapshot::probe(&remote_control, self.envs, self.project_root);
            let Err(block) = readiness.start_gate() else {
                return Ok(readiness);
            };
            if !self.offers_choice {
                return Err(self.refusal(block));
            }
            let mut stderr = render::err();
            write_prompt(&mut stderr, &block)?;
            match ask(&mut io::stdin().lock(), &mut stderr)? {
                Choice::RunOnly => {
                    waived.insert(AgentKind::new_unchecked(block.host.kind()));
                }
                Choice::ForGood => {
                    let updated = rimz::remote_control::persist_toggle(block.host, false)
                        .with_context(|| {
                            format!("turning {} remote control off", block.host.kind())
                        })?;
                    *machine_config = Arc::new(updated);
                }
                Choice::Quit => return Err(render::AlreadyReported.into()),
            }
        }
    }

    /// A live room cannot be born again, so its refusal carries no run-only hint.
    fn refusal(&self, block: StartBlock) -> anyhow::Error {
        if self.live {
            anyhow::Error::new(block.blocker)
        } else {
            anyhow::Error::new(block)
        }
    }
}

fn write_prompt(w: &mut impl Write, block: &StartBlock) -> io::Result<()> {
    let kind = block.host.kind();
    writeln!(w)?;
    render::write_blocker(w, &block.blocker, "blocked:")?;
    writeln!(w)?;
    writeln!(
        w,
        "  [t] start with {kind} remote control off for this run (default)"
    )?;
    writeln!(
        w,
        "  [p] turn it off for good: rimz config set remote_control.{kind} false"
    )?;
    writeln!(w, "  [q] quit")
}

/// Read one answer, asking again on anything unrecognised. End of input quits.
fn ask(input: &mut impl BufRead, output: &mut impl Write) -> io::Result<Choice> {
    let letters = ANSWERS.map(|(letter, _)| letter);
    loop {
        write!(output, "Choose [t/p/q] (t): ")?;
        output.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            return Ok(Choice::Quit);
        }
        if let Some(index) = crate::cli::parse_choice(&line, &letters, 0) {
            return Ok(ANSWERS[index].1);
        }
        writeln!(output, "Answer t, p, or q.")?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer(input: &str) -> (Choice, String) {
        let mut output = Vec::new();
        let choice = ask(&mut input.as_bytes(), &mut output).unwrap();
        (choice, String::from_utf8(output).unwrap())
    }

    #[test]
    fn answers_map_to_choices_and_unrecognised_input_asks_again() {
        assert_eq!(answer("\n").0, Choice::RunOnly);
        assert_eq!(answer("t\n").0, Choice::RunOnly);
        assert_eq!(answer("P\n").0, Choice::ForGood);
        assert_eq!(answer("q\n").0, Choice::Quit);
        assert_eq!(answer("").0, Choice::Quit, "end of input quits");

        let (choice, output) = answer("maybe\np\n");
        assert_eq!(choice, Choice::ForGood);
        assert_eq!(output.matches("Choose [t/p/q] (t): ").count(), 2);
        assert!(output.contains("Answer t, p, or q."), "{output}");
    }
}

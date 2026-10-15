//! Start preconditions and user-run remedies, independent of their presentation.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Surface {
    Plain,
    Privacy,
    Security,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub summary: String,
    pub command: Option<String>,
    pub surface: Surface,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocker {
    pub problem: String,
    pub fixes: Vec<Fix>,
}

impl Blocker {
    pub fn new(problem: impl Into<String>) -> Self {
        Self {
            problem: problem.into(),
            fixes: Vec::new(),
        }
    }

    pub fn with_fix(
        mut self,
        summary: impl Into<String>,
        command: Option<String>,
        surface: Surface,
    ) -> Self {
        self.fixes.push(Fix {
            summary: summary.into(),
            command,
            surface,
        });
        self
    }
}

impl std::fmt::Display for Blocker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.problem)?;
        for fix in &self.fixes {
            write!(f, "\n  - {}{}", fix.summary, fix.surface)?;
            if let Some(command) = &fix.command {
                write!(f, ": {command}")?;
            }
        }
        Ok(())
    }
}

/// The marker a fix's summary carries: empty for a plain fix.
impl std::fmt::Display for Surface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Plain => "",
            Self::Privacy => " [privacy; RimZ won't change this]",
            Self::Security => " [security; RimZ won't change this]",
        })
    }
}

impl std::error::Error for Blocker {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_keeps_commands_and_sensitive_surfaces_visible() {
        let blocker = Blocker::new("Remote control is blocked.")
            .with_fix(
                "Unset the privacy switch",
                Some("unset DISABLE_GROWTHBOOK".into()),
                Surface::Privacy,
            )
            .with_fix(
                "Grant folder trust",
                Some("rimz trust grant --agents claude".into()),
                Surface::Security,
            )
            .with_fix(
                "Upgrade Claude",
                Some("claude update".into()),
                Surface::Plain,
            );
        assert_eq!(
            blocker.to_string(),
            "Remote control is blocked.\n  - Unset the privacy switch [privacy; RimZ won't change this]: unset DISABLE_GROWTHBOOK\n  - Grant folder trust [security; RimZ won't change this]: rimz trust grant --agents claude\n  - Upgrade Claude: claude update"
        );
    }
}

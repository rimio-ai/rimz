//! Shared pane-first commands and answer receipts for actionable asks.

use rimz::agents::{AskKind, AskReply, AskRoute, OpenAskDetail};

pub(super) struct AskCommands<'a> {
    pub(super) detail: &'a OpenAskDetail,
    pub(super) pane: &'a str,
    pub(super) target: &'a str,
}

impl AskCommands<'_> {
    pub(super) fn focus(&self) -> String {
        format!("rimz agents focus {}", self.pane)
    }

    fn answer(&self) -> String {
        let questions = &self.detail.questions;
        let selector = if questions.len() > 1 {
            "<one selector per question>".to_owned()
        } else if let Some(question) = questions.first().filter(|q| !q.options.is_empty()) {
            if self.detail.open.kind == AskKind::Question {
                format!(
                    "<{}>",
                    (1..=question.options.len())
                        .map(|n| n.to_string())
                        .collect::<Vec<_>>()
                        .join("|")
                )
            } else {
                let option = &question.options[0];
                let caution = option
                    .caution
                    .as_ref()
                    .map(|text| format!("  ({text})"))
                    .unwrap_or_default();
                format!("{}{caution}", option.label)
            }
        } else {
            "--text \"<answer>\"".to_owned()
        };
        format!("rimz answer {} {selector}", self.target)
    }

    pub(super) fn next_step(&self) -> String {
        match self.detail.route {
            AskRoute::Pane => self.focus(),
            AskRoute::AsyncPane => format!(
                "{}, then Shift+Left  (it keeps working meanwhile)",
                self.focus()
            ),
            AskRoute::Shell if self.detail.open.kind == AskKind::Permission => {
                format!("{}  (or: {})", self.focus(), self.answer())
            }
            AskRoute::Shell if self.detail.questions.len() > 1 => {
                format!("rimz asks show {}, then {}", self.target, self.answer())
            }
            AskRoute::Shell => self.answer(),
        }
    }

    pub(super) fn show_footer(&self) -> String {
        match self.detail.route {
            AskRoute::Shell => {
                let mut footer = format!("answer here:  {}", self.answer());
                if let Some(actions) = self.detail.pane_actions {
                    let full_call = if self.detail.open.kind == AskKind::Permission {
                        ", and the full tool call"
                    } else {
                        ""
                    };
                    footer.push_str(&format!(
                        "\nin the pane:  {actions}{full_call}\n{}",
                        self.focus()
                    ));
                }
                footer
            }
            AskRoute::Pane => format!("answer in the pane:  {}", self.focus()),
            AskRoute::AsyncPane => {
                format!("answer in the pane:  {}, then Shift+Left", self.focus())
            }
        }
    }

    pub(super) fn pane_refusal(&self, who: &str) -> Option<String> {
        match self.detail.route {
            AskRoute::Shell => None,
            AskRoute::Pane => Some(format!(
                "{who}'s {} can only be answered in its pane: {}",
                self.detail.open.kind.short_label(),
                self.focus()
            )),
            AskRoute::AsyncPane => Some(format!(
                "{who}'s question is asked while it works and is answered in its pane: {}, then press Shift+Left",
                self.focus()
            )),
        }
    }

    pub(super) fn menu_refusal(&self) -> Option<String> {
        if self.detail.open.kind == AskKind::Question {
            return None;
        }
        let options = self
            .detail
            .questions
            .first()?
            .options
            .iter()
            .map(|option| format!("`{}`", option.label))
            .collect::<Vec<_>>()
            .join(" or ");
        let actions = self
            .detail
            .pane_actions
            .map(|actions| format!("{actions} stay in the pane"))
            .unwrap_or_else(|| "every other action stays in the pane".to_owned());
        Some(format!(
            "only {options} can be sent to this {} from here; {actions}: {}",
            self.detail.open.kind.short_label(),
            self.focus()
        ))
    }

    pub(super) fn missing_answer(&self, who: &str) -> String {
        let lines = self
            .detail
            .questions
            .iter()
            .enumerate()
            .map(|(index, question)| {
                let number = if self.detail.questions.len() > 1 {
                    format!("{}. ", index + 1)
                } else {
                    String::new()
                };
                let options = question
                    .options
                    .iter()
                    .enumerate()
                    .map(|(index, option)| format!("{}={}", index + 1, option.label))
                    .collect::<Vec<_>>()
                    .join(", ");
                let separator = if options.is_empty() { "" } else { ": " };
                format!(
                    "{number}{who} asks {:?}{separator}{options}",
                    question_line(self.detail.open.kind, &question.question)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        format!("{lines}\n  {}", self.next_step())
    }
}

pub(super) fn question_line(kind: AskKind, question: &str) -> &str {
    let mut lines = question.lines();
    let first = lines.next().unwrap_or_default();
    if kind == AskKind::PlanApproval {
        lines.find(|line| !line.trim().is_empty()).unwrap_or(first)
    } else {
        first
    }
}

pub(super) fn choice(detail: &OpenAskDetail, replies: &[AskReply]) -> String {
    detail
        .questions
        .iter()
        .zip(replies)
        .map(|(question, reply)| {
            let mut choices = reply
                .picks
                .iter()
                .map(|&pick| {
                    let option = &question.options[pick];
                    option
                        .caution
                        .as_ref()
                        .map(|caution| format!("{} ({caution})", option.label))
                        .unwrap_or_else(|| option.label.clone())
                })
                .collect::<Vec<_>>();
            if let Some(text) = &reply.text {
                choices.push(format!("{text:?}"));
            }
            choices.join(", ")
        })
        .collect::<Vec<_>>()
        .join("; ")
}

pub(super) fn unknown_ask(target: &str) -> String {
    format!(
        "no open ask `{target}`: it was answered, replaced, or never existed; rimz asks lists the open ones"
    )
}

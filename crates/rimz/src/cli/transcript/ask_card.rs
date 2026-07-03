use super::chat::write_body_lines;
use super::layout::*;
use super::*;

pub(super) fn write_ask_card(
    out: &mut impl Write,
    ask: &RenderEntry,
    answer: Option<&RenderEntry>,
) -> Result<()> {
    if ask.chat.questions.is_empty() {
        return write_text_card(out, ask, answer);
    }
    if !ask.chat.text.is_empty() {
        write_body_lines(out, &ask.chat.text)?;
    }
    write_structured_ask_card(out, &ask.chat.questions, answer)
}

pub(super) fn write_structured_ask_card(
    out: &mut impl Write,
    questions: &[rimz::chat::AskQuestion],
    answer: Option<&RenderEntry>,
) -> Result<()> {
    let (answers, source) = folded_answers(answer);
    write_structured_ask_card_with_answers(out, questions, &answers, source)
}

pub(super) fn write_structured_ask_card_with_answers(
    out: &mut impl Write,
    questions: &[rimz::chat::AskQuestion],
    answers: &[rimz::chat::AskAnswer],
    source: Option<&str>,
) -> Result<()> {
    let matched = match_question_answers(questions, answers);
    for (index, question) in questions.iter().enumerate() {
        if index > 0 {
            write_spine_blank(out, matched[index - 1].is_some())?;
        }
        write_question_block(out, question, matched[index].as_ref(), source)?;
    }
    Ok(())
}

pub(super) fn folded_answers(
    answer: Option<&RenderEntry>,
) -> (Vec<rimz::chat::AskAnswer>, Option<&str>) {
    let Some(answer) = answer else {
        return (Vec::new(), None);
    };
    (answer.chat.answers.clone(), Some(answer.chat.from.as_str()))
}

pub(super) fn match_question_answers(
    questions: &[rimz::chat::AskQuestion],
    answers: &[rimz::chat::AskAnswer],
) -> Vec<Option<rimz::chat::AskAnswer>> {
    let mut matched = vec![None; questions.len()];
    let mut used = vec![false; answers.len()];
    for (answer_index, answer) in answers.iter().enumerate() {
        let Some(question) = answer.question.as_deref() else {
            continue;
        };
        if let Some(question_index) = questions.iter().enumerate().position(|(index, candidate)| {
            candidate.question == question && matched[index].is_none()
        }) {
            matched[question_index] = Some(answer.clone());
            used[answer_index] = true;
        }
    }
    for (answer_index, answer) in answers.iter().enumerate() {
        if used[answer_index] {
            continue;
        }
        if let Some(slot) = matched.iter_mut().find(|slot| slot.is_none()) {
            *slot = Some(answer.clone());
        }
    }
    matched
}

pub(super) fn write_question_block(
    out: &mut impl Write,
    question: &rimz::chat::AskQuestion,
    answer: Option<&rimz::chat::AskAnswer>,
    source: Option<&str>,
) -> Result<()> {
    let answered = answer.is_some();
    write_question_text(out, answered, &question.question)?;
    match answer {
        Some(answer) if question.options.is_empty() => write_free_answer(out, answer, source),
        Some(answer) => write_option_answers(out, &question.options, answer, source),
        None => {
            for option in &question.options {
                write_wrapped_spine_fragments(
                    out,
                    false,
                    vec![StyledFragment::plain(format!("○ {}", option.label))],
                    "  ",
                )?;
                write_option_description(out, false, option)?;
            }
            write_unanswered(out)
        }
    }
}

pub(super) fn write_question_text(
    out: &mut impl Write,
    answered: bool,
    question: &str,
) -> Result<()> {
    for (index, line) in question.lines().enumerate() {
        let style = if index == 0 {
            Some(anstyle::Style::new().bold())
        } else {
            None
        };
        write_wrapped_spine_fragments(out, answered, vec![StyledFragment::prose(line, style)], "")?;
    }
    Ok(())
}

pub(super) fn write_free_answer(
    out: &mut impl Write,
    answer: &rimz::chat::AskAnswer,
    source: Option<&str>,
) -> Result<()> {
    let mut suffix_written = false;
    for choice in answer.chosen.iter().filter_map(|choice| non_empty(choice)) {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::prose(choice, Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) =
            answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

pub(super) fn write_option_answers(
    out: &mut impl Write,
    options: &[AskOption],
    answer: &rimz::chat::AskAnswer,
    source: Option<&str>,
) -> Result<()> {
    let chosen = answer
        .chosen
        .iter()
        .filter_map(|choice| non_empty(choice))
        .collect::<Vec<_>>();
    let mut suffix_written = false;
    for option in options {
        if chosen.contains(&option.label.as_str()) {
            let mut fragments = vec![
                StyledFragment::styled("●", render::palette::GOOD.bold()),
                StyledFragment::styled(option.label.clone(), render::palette::GOOD.bold()),
            ];
            if let Some(suffix) =
                answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
            {
                fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
            }
            write_wrapped_spine_fragments(out, true, fragments, "  ")?;
            write_option_description(out, true, option)?;
        } else {
            write_wrapped_spine_fragments(
                out,
                true,
                vec![StyledFragment::styled(
                    format!("○ {}", option.label),
                    render::palette::MUTED,
                )],
                "  ",
            )?;
            write_option_description(out, true, option)?;
        }
    }
    let other = chosen
        .into_iter()
        .filter(|choice| {
            !options
                .iter()
                .any(|option| option.label.as_str() == *choice)
        })
        .collect::<Vec<_>>();
    if !other.is_empty() {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::styled("other:", render::palette::MUTED),
            StyledFragment::prose(other.join(", "), Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) =
            answer_suffix_text(source, answer.note.as_deref(), &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

pub(super) fn write_option_description(
    out: &mut impl Write,
    answered: bool,
    option: &AskOption,
) -> Result<()> {
    let Some(description) = option.description.as_deref().and_then(non_empty) else {
        return Ok(());
    };
    for line in description.lines().filter_map(non_empty) {
        write_wrapped_spine_fragments_with_first_indent(
            out,
            answered,
            vec![StyledFragment::styled(line, render::palette::FAINT)],
            "    ",
            "    ",
        )?;
    }
    Ok(())
}

pub(super) fn write_text_card(
    out: &mut impl Write,
    ask: &RenderEntry,
    answer: Option<&RenderEntry>,
) -> Result<()> {
    let answered = answer.is_some();
    for line in ask.chat.text.lines() {
        write_wrapped_spine_fragments(out, answered, vec![StyledFragment::prose(line, None)], "")?;
    }
    let Some(answer) = answer else {
        return write_unanswered(out);
    };
    let text = if answer.chat.answers.is_empty() {
        answer.chat.text.trim().to_owned()
    } else {
        rimz::chat::answers_text(&answer.chat.answers)
    };
    if text.is_empty() {
        return Ok(());
    }
    let mut suffix_written = false;
    for line in text.lines() {
        let mut fragments = vec![
            StyledFragment::styled("●", render::palette::GOOD.bold()),
            StyledFragment::prose(line, Some(render::palette::GOOD.bold())),
        ];
        if let Some(suffix) = answer_suffix_text(Some(&answer.chat.from), None, &mut suffix_written)
        {
            fragments.push(StyledFragment::styled(suffix, render::palette::MUTED));
        }
        write_wrapped_spine_fragments(out, true, fragments, "  ")?;
    }
    Ok(())
}

pub(super) fn write_unanswered(out: &mut impl Write) -> Result<()> {
    write_wrapped_spine_fragments(
        out,
        false,
        vec![StyledFragment::styled(
            "◌ unanswered",
            render::palette::WARN,
        )],
        "",
    )
}

pub(super) fn answer_suffix_text(
    source: Option<&str>,
    note: Option<&str>,
    written: &mut bool,
) -> Option<String> {
    if *written {
        return None;
    }
    *written = true;
    let source = source.and_then(non_empty)?;
    let mut suffix = format!(" — {source}");
    if let Some(note) = note.and_then(non_empty) {
        suffix.push_str(" · “");
        suffix.push_str(note);
        suffix.push('”');
    }
    Some(suffix)
}

pub(super) fn non_empty(text: &str) -> Option<&str> {
    let text = text.trim();
    (!text.is_empty()).then_some(text)
}

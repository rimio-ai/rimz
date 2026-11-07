use super::*;

#[test]
fn terminal_history_dates_its_end_and_every_row_clock() {
    let spec = toml::from_str(
        "[workflow]\nscope='worktree'\nstart='pr'\n\
         [checks.pr]\nrun='true'\non.done='@done'\n",
    )
    .unwrap();
    let mut record = waiting_record(0, 0, 1);
    record.state = records::RunState::Done;
    record.terminal_at = Some(at(12));
    let mut out = anstream::AutoStream::new(Vec::new(), anstream::ColorChoice::Never);
    workflow_view::history::terminal(
        &record,
        &spec,
        &[evaluation(1, 10, "done", None)],
        render::prose::Prose::Raw,
        &mut out,
    )
    .unwrap();
    let text = String::from_utf8(out.into_inner()).unwrap();
    let zone = jiff::tz::TimeZone::system();
    let end = at(12)
        .to_zoned(zone.clone())
        .strftime("%Y-%m-%d %H:%M")
        .to_string();
    let row = at(10).to_zoned(zone).strftime("%Y-%m-%d %H:%M").to_string();
    assert!(
        text.lines()
            .nth(1)
            .unwrap()
            .ends_with(&format!("done {end}")),
        "{text:?}; expected end {end:?}"
    );
    assert!(text.contains(&format!("  {row} #1 pr said done")), "{text}");
}

#[test]
fn digests_print_only_the_first_twelve_hex_characters() {
    let mut out = Vec::new();
    digests_out(
        &mut out,
        &BTreeMap::from([
            (
                "prompt.md".into(),
                "0123456789abcdef0123456789abcdef".into(),
            ),
            (
                "checks/pr.sh".into(),
                "abcdef0123456789abcdef0123456789".into(),
            ),
        ]),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "    checks/pr.sh: abcdef012345\n    prompt.md: 0123456789ab\n"
    );
}

#[test]
fn empty_child_groups_print_nothing() {
    let mut out = Vec::new();
    children_out(&mut out, &Children::default()).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "");
}

#[test]
fn unread_fields_name_their_shared_cause_once_without_rewriting_other_reasons() {
    let actual: Vec<_> = [
        "ci (probed before this check was entered), pr (probed before this check was entered)",
        "ci (not read), pr (not read)",
        "ci (head mismatch), pr (not read)",
        "a script's own reason",
    ]
    .into_iter()
    .map(|reason| {
        let mut entry = evaluation(1, 0, "unknown", None);
        let JournalEvent::Evaluation { object, .. } = &mut entry.event else {
            unreachable!();
        };
        *object = json!({"reason":reason,"readings":{"ci":null,"pr":null}});
        history(&[entry])
            .lines()
            .find(|line| line.contains("reason:"))
            .unwrap()
            .trim()
            .to_owned()
    })
    .collect();
    assert_eq!(
        actual,
        [
            "reason: ci, pr (probed before this check was entered)",
            "reason: ci, pr (not read)",
            "reason: ci (head mismatch), pr (not read)",
            "reason: a script's own reason",
        ]
    );
}

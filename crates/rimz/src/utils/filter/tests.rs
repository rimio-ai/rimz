use super::*;

fn terms(input: &str) -> Result<Vec<Term<'_>>, TermError> {
    let mut rest = input;
    std::iter::from_fn(|| next_term(&mut rest)).collect()
}

fn text(word: &str) -> Word {
    Word::Text(word.into())
}

#[test]
fn terms_split_on_whitespace_and_values_on_commas() {
    let lexed = terms("  kind=a,\"b c\"   loop-task!=*,NONE age<=2d ").unwrap();
    assert_eq!(
        lexed,
        [
            Term {
                text: "kind=a,\"b c\"",
                field: "kind",
                op: Op::Eq,
                values: vec![text("a"), text("b c")],
            },
            Term {
                text: "loop-task!=*,NONE",
                field: "loop-task",
                op: Op::Ne,
                values: vec![Word::Any, Word::None],
            },
            Term {
                text: "age<=2d",
                field: "age",
                op: Op::Le,
                values: vec![text("2d")],
            },
        ]
    );
    assert_eq!(
        terms("x=\"*\",\"none\"").unwrap()[0].values,
        [text("*"), text("none")],
        "a quoted grammar word is plain text"
    );
    assert!(terms("   ").unwrap().is_empty());
}

#[test]
fn grammar_refusals_name_the_term() {
    for (input, expected) in [
        (
            "kind",
            "filter term `kind` has no operator: use one of = != > >= < <=",
        ),
        ("=x", "filter term `=x` has no field before its operator"),
        ("kind=", "filter term `kind=` has an empty value"),
        ("kind=a,", "filter term `kind=a,` has an empty value"),
        (
            "kind=\"open",
            "filter term `kind=\"open` has an unterminated quote",
        ),
        (
            "kind=a\"b\"",
            "filter term `kind=a\"b\"` has a quote inside a word",
        ),
        ("ok=1 bad", "filter term `bad` has no operator"),
    ] {
        let message = terms(input).unwrap_err().to_string();
        assert!(message.starts_with(expected), "{input}: {message}");
    }
}

#[test]
fn operators_hold_any_of_and_none_of_and_compare_in_order() {
    let values = [1, 2];
    assert!(Op::Eq.holds(&values, |value| *value == 2));
    assert!(!Op::Ne.holds(&values, |value| *value == 2));
    assert!(Op::Ne.holds(&values, |value| *value == 3));
    assert!(Op::Ge.compares(2, 2) && !Op::Gt.compares(2, 2));
    assert!(Op::Le.compares(1.0, 2.0) && Op::Lt.compares(1.0, 2.0));
    assert!(Op::Gt.ordered() && !Op::Ne.ordered());
}

#[test]
fn a_wildcard_stands_for_any_run_of_characters() {
    assert!(glob("*expiry*", "token expiry fix"));
    assert!(glob("a*c", "abc") && glob("a*c", "ac"));
    assert!(!glob("a*c", "abcd"));
    assert!(glob("exact", "exact") && !glob("exact", "exactly"));
    assert!(glob("*", ""));
}

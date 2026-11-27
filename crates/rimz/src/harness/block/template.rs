//! The answer `rimz workflow try` offers for a call it has none for: the result the call's block
//! gives, read from that result's schema with a placeholder in each value.

use serde_json::{Map, Value};

use super::{Agent, Block, BlockName, Now, Schedule, Shell, Team, Watch, Workflow, schema};
use crate::harness::when::WhenExpr;

/// The result a call of `block` with `object` gives, each value a placeholder: an empty string,
/// zero, `false`, an enum's first value, or an empty list or map. An agent's call with a
/// `schema` gives that schema's answer, and a watch's `readings` name each key its condition
/// reads, unread.
pub fn template(block: BlockName, object: &Value) -> Value {
    let result = match block {
        BlockName::Agent => object
            .get("schema")
            .cloned()
            .unwrap_or_else(schema::<<Agent as Block>::Result>),
        BlockName::Team => schema::<<Team as Block>::Result>(),
        BlockName::Shell => schema::<<Shell as Block>::Result>(),
        BlockName::Watch => schema::<<Watch as Block>::Result>(),
        BlockName::Workflow => schema::<<Workflow as Block>::Result>(),
        BlockName::Schedule => schema::<<Schedule as Block>::Result>(),
        BlockName::Now => schema::<<Now as Block>::Result>(),
    };
    let mut answer = placeholder(&result);
    if let Some(Value::Object(readings)) = answer.get_mut("readings")
        && let Some(condition) = object
            .get("when")
            .and_then(Value::as_str)
            .and_then(|when| WhenExpr::parse(&[when.to_owned()]).ok())
    {
        readings.extend(
            condition
                .terms()
                .map(|term| (term.key.clone(), Value::Null)),
        );
    }
    answer
}

fn placeholder(schema: &Value) -> Value {
    let Some(map) = schema.as_object() else {
        return Value::Null;
    };
    if let Some(first) = map
        .get("enum")
        .and_then(Value::as_array)
        .and_then(|values| values.first())
    {
        return first.clone();
    }
    let kind = match map.get("type") {
        Some(Value::Array(kinds)) => kinds.first().and_then(Value::as_str),
        kind => kind.and_then(Value::as_str),
    };
    match kind {
        Some("string") => Value::String(String::new()),
        Some("integer" | "number") => Value::from(0),
        Some("boolean") => Value::Bool(false),
        Some("array") => Value::Array(Vec::new()),
        Some("object") => Value::Object(
            map.get("properties")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .map(|(field, property)| (field.clone(), placeholder(property)))
                .collect::<Map<_, _>>(),
        ),
        _ => Value::Null,
    }
}

//! The TypeScript declarations `rimz workflow types` prints for the globals of a workflow
//! script. Each block's object, its result, and the failure are printed from their schemas, so
//! a field added to a block shows here without a second edit.

use serde_json::{Map, Value};

use super::{
    Agent, Block, BlockName, Failure, Now, Schedule, Shell, Team, Watch, Workflow, schema,
};

const GLOBALS: &str = "\
// The globals of a RimZ workflow script, printed by `rimz workflow types`.

/** What the start passed, or `undefined`. */
declare const args: any;

/** The text of a file of the workflow's directory; a path that leaves the directory is refused. */
declare function file(path: string): string;

/**
 * Runs every item through the stages in turn and gives one result per item, in the order of
 * the items. A stage is called with the result of the stage before it (the item itself, for the
 * first stage), the item, and its index. A stage that throws leaves `null` in its item's place.
 */
declare function pipeline<T>(
  items: T[],
  stage: (previous: any, item: T, index: number) => unknown,
  ...stages: ((previous: any, item: T, index: number) => unknown)[]
): Promise<any[]>;

/**
 * Calls every task and gives their results in the order of the list. A task that throws leaves
 * `null` in its place.
 */
declare function parallel(tasks: (() => unknown)[]): Promise<any[]>;

/**
 * The milliseconds of a duration in the grammar `timeout` takes (`90s`, `30m`, `2h`, `3d`);
 * one that does not parse throws. It makes no call.
 */
declare function ms(duration: string): number;

/**
 * Whether a condition in the grammar `watch` takes reads true over `readings`, decided as a
 * watch decides it: three-valued, `null` for unknown. A key absent or `null` is not read, so
 * `unread || true` is true, `false && unread` false, and `!unread` unknown. A condition that
 * does not parse throws. It makes no call.
 */
declare function reads(when: string, readings: Record<string, string | null>): boolean | null;

/**
 * Watches until a case of `list` wins, and gives its `outcome` with the readings of the look it
 * won on. The first case in order whose `when` reads true wins. A case with no `outcome` waits:
 * while it reads true, no case after it wins. A case with `for` wins once that long passes
 * without it reading false or a case before it reading true, so an unread stretch counts, and a
 * look after the hold still has it win. `label` and `cwd` go to each watch; `timeout` is one
 * deadline for the whole call, read with a `now` call before each watch. A list it cannot decide
 * throws before any call.
 */
declare function cases(
  list: Case[],
  options?: { label?: string; cwd?: string; timeout?: string },
): Promise<CasesResult>;

/** One case of `cases`. */
interface Case {
  /** A condition in the grammar `watch` takes. */
  when: string;
  /** What `cases` gives when this case wins; a case without one waits. */
  outcome?: string;
  /** How long the case must go without reading false, or a case before it true, to win (`30m`). */
  for?: string;
}

/** What `cases` gives. */
interface CasesResult {
  /** The `outcome` of the case that won. */
  outcome: string;
  /** The reading of each key the condition names, at the look the case won on: `null` unread. */
  readings: Record<string, string | null>;
}
";

/// The declarations of every global a workflow script reaches: `args`, `file`, `pipeline`,
/// `parallel`, `ms`, `reads`, `cases`, the seven blocks with their objects and results, and the
/// failure a call throws.
pub fn typescript() -> String {
    let mut out = String::from(GLOBALS);
    declare_block::<Agent>(&mut out);
    declare_block::<Team>(&mut out);
    declare_block::<Shell>(&mut out);
    declare_block::<Watch>(&mut out);
    declare_block::<Workflow>(&mut out);
    declare_block::<Schedule>(&mut out);
    declare_block::<Now>(&mut out);
    let failure = schema::<Failure>();
    out.push('\n');
    doc(&mut out, &failure, "");
    interface(&mut out, "Failure", &failure, " extends Error");
    out
}

/// A block's function, its object without the fields a script may not pass, and its result.
fn declare_block<B: Block>(out: &mut String) {
    let name = B::NAME.as_str();
    let object = pascal(name);
    let mut object_schema = schema::<B>();
    if let Some(properties) = object_schema
        .get_mut("properties")
        .and_then(Value::as_object_mut)
    {
        properties.retain(|field, _| !B::SCRIPT_REFUSED.contains(&field.as_str()));
        for (field, refused) in B::SCRIPT_REFUSED_VALUES {
            if let Some(Value::Array(values)) = properties
                .get_mut(*field)
                .and_then(|property| property.get_mut("enum"))
            {
                values.retain(|value| value != refused);
            }
        }
    }
    let result_schema = schema::<B::Result>();
    let result = if result_schema.get("properties").is_some() {
        format!("{object}Result")
    } else {
        match type_of(&result_schema, "").as_str() {
            "unknown" => "any".to_owned(),
            known => known.to_owned(),
        }
    };
    out.push('\n');
    doc(out, &object_schema, "");
    if B::BARE {
        out.push_str(&format!("declare function {name}(): Promise<{result}>;\n"));
        return;
    }
    if B::NAME == BlockName::Agent {
        out.push_str(&format!(
            "declare function {name}(block: {object} & {{ schema: object }}): Promise<any>;\n"
        ));
    }
    out.push_str(&format!(
        "declare function {name}(block: {object}): Promise<{result}>;\n"
    ));
    out.push('\n');
    interface(out, &object, &object_schema, "");
    if result != "any" {
        out.push('\n');
        interface(out, &result, &result_schema, "");
    }
}

fn interface(out: &mut String, name: &str, schema: &Value, extends: &str) {
    out.push_str(&format!("interface {name}{extends} "));
    out.push_str(&object_type(schema, ""));
    out.push('\n');
}

/// A schema's `description` as a doc comment at `indent`.
fn doc(out: &mut String, schema: &Value, indent: &str) {
    let Some(description) = schema.get("description").and_then(Value::as_str) else {
        return;
    };
    if !description.contains('\n') {
        out.push_str(&format!("{indent}/** {description} */\n"));
        return;
    }
    out.push_str(&format!("{indent}/**\n"));
    for line in description.lines() {
        out.push_str(format!("{indent} * {line}").trim_end());
        out.push('\n');
    }
    out.push_str(&format!("{indent} */\n"));
}

/// An object schema with properties as a type literal: the required fields first, in the
/// order the schema requires them, then the optional ones in the schema's order.
fn object_type(schema: &Value, indent: &str) -> String {
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    let required: Vec<&str> = schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|field| properties.contains_key(*field))
        .collect();
    let optional = properties
        .keys()
        .map(String::as_str)
        .filter(|field| !required.contains(field));
    let inner = format!("{indent}  ");
    let mut out = String::from("{\n");
    for field in required.iter().copied().chain(optional) {
        let property = &properties[field];
        doc(&mut out, property, &inner);
        let mark = if required.contains(&field) { "" } else { "?" };
        out.push_str(&format!(
            "{inner}{field}{mark}: {};\n",
            type_of(property, &inner)
        ));
    }
    out.push_str(&format!("{indent}}}"));
    out
}

fn type_of(schema: &Value, indent: &str) -> String {
    let Some(map) = schema.as_object() else {
        return "unknown".to_owned();
    };
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        let values: Vec<String> = values.iter().map(Value::to_string).collect();
        return values.join(" | ");
    }
    match map.get("type") {
        Some(Value::Array(kinds)) => kinds
            .iter()
            .map(|kind| kind_type(schema, kind.as_str(), indent))
            .collect::<Vec<_>>()
            .join(" | "),
        kind => kind_type(schema, kind.and_then(Value::as_str), indent),
    }
}

/// The type of `schema` read as the one JSON type `kind`.
fn kind_type(schema: &Value, kind: Option<&str>, indent: &str) -> String {
    let Some(map) = schema.as_object() else {
        return "unknown".to_owned();
    };
    match kind {
        Some("null") => "null".to_owned(),
        Some("string") => "string".to_owned(),
        Some("integer" | "number") => "number".to_owned(),
        Some("boolean") => "boolean".to_owned(),
        Some("array") => {
            let item = map
                .get("items")
                .map_or_else(|| "unknown".to_owned(), |items| type_of(items, indent));
            if item.contains(' ') && !item.starts_with('{') {
                format!("({item})[]")
            } else {
                format!("{item}[]")
            }
        }
        Some("object") if map.contains_key("properties") => object_type(schema, indent),
        Some("object") => {
            let value = map
                .get("additionalProperties")
                .map_or_else(|| "unknown".to_owned(), |value| type_of(value, indent));
            format!("Record<string, {value}>")
        }
        _ => "unknown".to_owned(),
    }
}

fn pascal(name: &str) -> String {
    let mut chars = name.chars();
    chars
        .next()
        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

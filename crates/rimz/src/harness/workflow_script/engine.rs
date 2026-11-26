//! The sandboxed QuickJS context one pass runs in, and the pass: the body runs until nothing more
//! can run, then each line of the log settles its call's promise and the body runs again.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::time::Instant;

use rquickjs::context::{EvalOptions, intrinsic};
use rquickjs::function::Opt;
use rquickjs::promise::PromiseState;
use rquickjs::{
    CatchResultExt as _, CaughtError, Context, Ctx, Exception, Function, Persistent, Promise,
    Runtime, Value as JsValue,
};
use serde_json::Value;

use super::heap::{Capped, Heap};
use super::load::Loaded;
use super::{Call, Caps, End, Log, Outcome, Pass, Replay, pass_failure};
use crate::harness::block::{
    Agent, Block, BlockName, Caller, Cause, Failure, Schedule, Shell, Team, Watch, Workflow,
    digest, parse,
};

const PRELUDE: &str = include_str!("prelude.js");

/// The name the prelude's own frames carry, apart from the script's.
const PRELUDE_NAME: &str = "<rimz>";

type Settlers = (Persistent<Function<'static>>, Persistent<Function<'static>>);

/// What the block functions and the rejection tracker share with the pass.
#[derive(Default)]
struct State {
    calls: Vec<Call>,
    /// The calls no line has answered yet, with their promise's settle functions.
    open: BTreeMap<u64, Settlers>,
    /// A recorded log's block and digest per call number.
    expected: BTreeMap<u64, (BlockName, String)>,
    mismatch: Option<Failure>,
    /// Rejected promises nothing handles yet, with their reasons, in the order they were rejected.
    unhandled: Vec<(Persistent<JsValue<'static>>, Persistent<JsValue<'static>>)>,
    /// The prelude's maker of a failure's `Error`.
    fail: Option<Persistent<Function<'static>>>,
}

impl State {
    fn track<'js>(
        &mut self,
        ctx: &Ctx<'js>,
        promise: JsValue<'js>,
        reason: JsValue<'js>,
        handled: bool,
    ) {
        if !handled {
            let entry = (
                Persistent::save(ctx, promise),
                Persistent::save(ctx, reason),
            );
            self.unhandled.push(entry);
            return;
        }
        self.unhandled.retain(|(tracked, _)| {
            tracked
                .clone()
                .restore(ctx)
                .is_ok_and(|tracked| tracked != promise)
        });
    }
}

pub(super) fn run(pass: &Pass<'_>, loaded: Loaded) -> Result<Replay, String> {
    let failed = |error: rquickjs::Error| format!("the script engine did not start: {error}");
    let heap = Heap::new();
    let runtime = Runtime::new_with_alloc(Capped(Rc::clone(&heap))).map_err(failed)?;
    let context = Context::builder()
        .with::<(
            intrinsic::Date,
            intrinsic::Eval,
            intrinsic::RegExpCompiler,
            intrinsic::RegExp,
            intrinsic::Json,
            intrinsic::Proxy,
            intrinsic::MapSet,
            intrinsic::Promise,
        )>()
        .build(&runtime)
        .map_err(failed)?;
    heap.cap(pass.caps.memory);
    let interrupted = Rc::new(Cell::new(false));
    let deadline = Instant::now() + pass.caps.time;
    let flag = Rc::clone(&interrupted);
    runtime.set_interrupt_handler(Some(Box::new(move || {
        let over = Instant::now() >= deadline;
        flag.set(flag.get() || over);
        over
    })));
    let mut state = State::default();
    if let Log::Recorded(lines) = pass.log {
        for line in lines {
            let twice = state
                .expected
                .insert(line.number, (line.block, line.digest.clone()))
                .is_some();
            if twice && state.mismatch.is_none() {
                let reason = format!("the log answers call {} twice", line.number);
                state.mismatch = Some(pass_failure(Cause::Replay, reason, Some(line.number)));
            }
        }
    }
    let state = Rc::new(RefCell::new(state));
    let tracker = Rc::clone(&state);
    runtime.set_host_promise_rejection_tracker(Some(Box::new(
        move |ctx, promise, reason, handled| {
            tracker.borrow_mut().track(&ctx, promise, reason, handled);
        },
    )));
    let engine = Engine {
        context: &context,
        runtime: &runtime,
        state: &state,
        interrupted: &interrupted,
        heap: &heap,
        caps: pass.caps,
        name: pass.name,
    };
    let replayed = engine.replay(pass, &loaded.body);
    // Every engine value the state holds must go before the context does.
    let (calls, open, held) = {
        let mut state = state.borrow_mut();
        let open = state.open.keys().copied().collect::<Vec<_>>();
        let held = (
            std::mem::take(&mut state.unhandled),
            std::mem::take(&mut state.open),
            state.fail.take(),
        );
        (std::mem::take(&mut state.calls), open, held)
    };
    drop(held);
    let (end, used) = replayed?;
    Ok(Replay {
        meta: loaded.meta,
        calls,
        open,
        used,
        end,
    })
}

struct Engine<'a> {
    context: &'a Context,
    runtime: &'a Runtime,
    state: &'a Rc<RefCell<State>>,
    interrupted: &'a Cell<bool>,
    heap: &'a Heap,
    caps: Caps,
    name: &'a str,
}

impl Engine<'_> {
    fn replay(&self, pass: &Pass<'_>, body: &str) -> Result<(End, usize), String> {
        let body = match self.start(pass, body)? {
            Ok(body) => body,
            Err(end) => return Ok((end, 0)),
        };
        if let Some(end) = self.pump() {
            return Ok((end, 0));
        }
        let lines: Vec<(u64, &Outcome)> = match pass.log {
            Log::Recorded(lines) => lines
                .iter()
                .map(|line| (line.number, &line.outcome))
                .collect(),
            Log::Answers(answers) => (1..).zip(answers).collect(),
        };
        let recorded = matches!(pass.log, Log::Recorded(_));
        let mut used = 0;
        for (number, outcome) in lines {
            if self.settled(&body) || self.state.borrow().mismatch.is_some() {
                break;
            }
            let taken = {
                let mut state = self.state.borrow_mut();
                let block = number
                    .checked_sub(1)
                    .and_then(|index| state.calls.get(usize::try_from(index).ok()?))
                    .map(|call| call.block);
                match (state.open.remove(&number), block) {
                    (Some(settlers), Some(block)) => Some((settlers, block)),
                    // A recorded line answers a call the run made, so one the replay has not
                    // made is a script that differs; an answers file just runs out of calls.
                    _ if !recorded && state.open.is_empty() => None,
                    (_, made) => {
                        let reason = if made.is_some() {
                            format!("the log answers call {number} twice")
                        } else {
                            format!("the log answers call {number}, which the script has not made")
                        };
                        state.mismatch = Some(pass_failure(Cause::Replay, reason, Some(number)));
                        None
                    }
                }
            };
            let Some((settlers, block)) = taken else {
                break;
            };
            used += 1;
            if let Some(end) = self
                .settle(number, block, outcome, settlers)
                .or_else(|| self.pump())
            {
                return Ok((end, used));
            }
        }
        Ok((self.end(body), used))
    }

    /// Build the globals, run the prelude, and start the body: the body's promise, or how the
    /// pass ended before it had one. An error is a body that does not compile.
    fn start(
        &self,
        pass: &Pass<'_>,
        body: &str,
    ) -> Result<Result<Persistent<Promise<'static>>, End>, String> {
        self.context.with(|ctx| {
            if let Err(error) = install(&ctx, self.state, pass).catch(&ctx) {
                return Ok(Err(self.caught(&ctx, error)));
            }
            let source = format!("(async function () {{{body}\n}})()");
            match ctx
                .eval_with_options::<JsValue, _>(source, options(self.name))
                .catch(&ctx)
            {
                Ok(value) => match value.into_promise() {
                    Some(promise) => Ok(Ok(Persistent::save(&ctx, promise))),
                    None => Err(
                        "the body's braces do not balance: it closes the function it runs in"
                            .to_owned(),
                    ),
                },
                Err(CaughtError::Exception(error))
                    if error
                        .get::<_, String>("name")
                        .is_ok_and(|name| name == "SyntaxError") =>
                {
                    Err(self.syntax(body, &error))
                }
                Err(error) => Ok(Err(self.caught(&ctx, error))),
            }
        })
    }

    /// Run every job the engine has queued, or say how the pass ended in one.
    fn pump(&self) -> Option<End> {
        loop {
            match self.runtime.execute_pending_job() {
                Ok(true) => {}
                Ok(false) => return None,
                Err(job) => {
                    return Some(job.0.with(|ctx| {
                        let thrown = ctx.catch();
                        self.thrown(&ctx, thrown)
                    }));
                }
            }
        }
    }

    fn settled(&self, body: &Persistent<Promise<'static>>) -> bool {
        self.context.with(|ctx| {
            body.clone()
                .restore(&ctx)
                .map_or(true, |body| body.state() != PromiseState::Pending)
        })
    }

    /// Hand `outcome` back to call `number`, filling a failure's block and step, or say how the
    /// pass ended in doing so.
    fn settle(
        &self,
        number: u64,
        block: BlockName,
        outcome: &Outcome,
        (resolve, reject): Settlers,
    ) -> Option<End> {
        self.context.with(|ctx| {
            let settled = match outcome {
                Outcome::Result(value) => resolve.restore(&ctx).and_then(|resolve| {
                    resolve.call::<_, ()>((ctx.json_parse(value.to_string())?,))
                }),
                Outcome::Failure(failure) => {
                    let failure = Failure {
                        block: Some(block),
                        step: Some(number),
                        ..failure.clone()
                    };
                    reject.restore(&ctx).and_then(|reject| {
                        reject.call::<_, ()>((failure_error(&ctx, self.state, &failure)?,))
                    })
                }
            };
            settled
                .catch(&ctx)
                .err()
                .map(|error| self.caught(&ctx, error))
        })
    }

    /// How the pass ended, a cap first: a script that caught the cap's error still passed it.
    fn end(&self, body: Persistent<Promise<'static>>) -> End {
        let end = self.body_end(body);
        self.cap().unwrap_or(end)
    }

    fn body_end(&self, body: Persistent<Promise<'static>>) -> End {
        if let Some(failure) = self.state.borrow_mut().mismatch.take() {
            return End::Failed(failure);
        }
        self.context.with(|ctx| {
            let body = match body.restore(&ctx) {
                Ok(body) => body,
                Err(error) => return self.caught(&ctx, CaughtError::Error(error)),
            };
            match body.state() {
                PromiseState::Resolved => {
                    if let Some(end) = self.unhandled(&ctx) {
                        return end;
                    }
                    let returned = body
                        .result::<JsValue>()
                        .unwrap_or_else(|| Ok(JsValue::new_undefined(ctx.clone())));
                    match returned.catch(&ctx).map(|value| json_of(&ctx, value)) {
                        Ok(Ok(value)) => End::Returned(value),
                        Ok(Err(reason)) => End::Threw(reason),
                        Err(error) => self.caught(&ctx, error),
                    }
                }
                PromiseState::Rejected => {
                    match body.result::<JsValue>().map(|result| result.catch(&ctx)) {
                        Some(Err(error)) => self.caught(&ctx, error),
                        Some(Ok(value)) => self.thrown(&ctx, value),
                        None => self.caught(&ctx, CaughtError::Error(rquickjs::Error::Unknown)),
                    }
                }
                PromiseState::Pending if !self.state.borrow().open.is_empty() => End::Pending,
                PromiseState::Pending => self.unhandled(&ctx).unwrap_or_else(|| {
                    let reason = "the script awaits something no call will settle".to_owned();
                    End::Failed(pass_failure(Cause::Stuck, reason, None))
                }),
            }
        })
    }

    /// How the run ends for the first rejection nothing handled: a failed call never awaited.
    fn unhandled<'js>(&self, ctx: &Ctx<'js>) -> Option<End> {
        let reason = self
            .state
            .borrow()
            .unhandled
            .first()
            .map(|(_, reason)| reason.clone())?;
        let reason = reason.restore(ctx).ok()?;
        Some(self.thrown(ctx, reason))
    }

    fn caught<'js>(&self, ctx: &Ctx<'js>, error: CaughtError<'js>) -> End {
        match error {
            CaughtError::Exception(error) => self.thrown(ctx, error.into_value()),
            CaughtError::Value(value) => self.thrown(ctx, value),
            CaughtError::Error(error) => self.cap().unwrap_or_else(|| {
                End::Failed(pass_failure(
                    Cause::Invalid,
                    format!("the script engine failed: {error}"),
                    None,
                ))
            }),
        }
    }

    /// The cap this pass passed, if any: the interrupt handler's deadline, or an allocation the
    /// heap refused, whatever the script caught after.
    fn cap(&self) -> Option<End> {
        let reason = if self.interrupted.get() {
            format!(
                "the script ran past the {}s time cap of one pass",
                self.caps.time.as_secs_f64()
            )
        } else if self.heap.refused() {
            format!(
                "the script passed the {} MiB memory cap of one pass",
                self.caps.memory >> 20
            )
        } else {
            return None;
        };
        Some(End::Failed(pass_failure(Cause::Invalid, reason, None)))
    }

    /// How the run ends for a thrown value: a cap, a call's failure, or the script's own error.
    fn thrown<'js>(&self, ctx: &Ctx<'js>, value: JsValue<'js>) -> End {
        let end = self.thrown_end(ctx, value);
        self.cap().unwrap_or(end)
    }

    fn thrown_end<'js>(&self, ctx: &Ctx<'js>, value: JsValue<'js>) -> End {
        let error = value
            .as_object()
            .and_then(|object| Exception::from_object(object.clone()));
        if let Ok(Value::Object(fields)) = json_of(ctx, value.clone())
            && fields.contains_key("block")
            && let Ok(failure) = serde_json::from_value::<Failure>(Value::Object(fields))
        {
            return End::Failed(failure);
        }
        End::Threw(match error {
            Some(error) => self.describe(&error),
            None => value
                .get::<rquickjs::convert::Coerced<String>>()
                .map_or_else(|_| "a value that is not text".to_owned(), |text| text.0),
        })
    }

    /// An error's message and the script line it was thrown at.
    fn describe(&self, error: &Exception<'_>) -> String {
        let name = error.get::<_, String>("name").unwrap_or_default();
        let message = error.message().unwrap_or_default();
        let text = if name.is_empty() || name == "Error" {
            message
        } else {
            format!("{name}: {message}")
        };
        match error.stack().and_then(|stack| line_of(&stack, self.name)) {
            Some(line) => format!("{text} ({}:{line})", self.name),
            None => text,
        }
    }

    /// A body that does not compile, at its line.
    fn syntax(&self, body: &str, error: &Exception<'_>) -> String {
        let message = error.message().unwrap_or_default();
        let Some(line) = error.stack().and_then(|stack| line_of(&stack, self.name)) else {
            return message;
        };
        let source = body
            .lines()
            .nth(line.saturating_sub(1))
            .unwrap_or_default()
            .trim_start();
        let statement = |word: &str| {
            source.strip_prefix(word).is_some_and(|rest| {
                !rest.starts_with(|char: char| char.is_alphanumeric() || char == '_' || char == '$')
                    && !rest.trim_start().starts_with('(')
            })
        };
        if statement("import") {
            format!(
                "line {line}: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `file`, and `args`"
            )
        } else if statement("export") {
            format!("line {line}: a workflow script exports only `meta`, in its first statement")
        } else {
            format!("line {line}: {message}")
        }
    }
}

fn options(name: &str) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = true;
    options.filename = Some(name.to_owned());
    options
}

/// The script's globals: the prelude's, the six blocks, `file`, and `args`.
fn install<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    pass: &Pass<'_>,
) -> rquickjs::Result<()> {
    let fail = ctx.eval_with_options::<Function, _>(PRELUDE, options(PRELUDE_NAME))?;
    state.borrow_mut().fail = Some(Persistent::save(ctx, fail));
    block::<Agent>(ctx, state)?;
    block::<Team>(ctx, state)?;
    block::<Shell>(ctx, state)?;
    block::<Watch>(ctx, state)?;
    block::<Workflow>(ctx, state)?;
    block::<Schedule>(ctx, state)?;
    let globals = ctx.globals();
    let dir = pass.dir.map(Path::to_path_buf);
    let file = Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let read = match &dir {
            Some(dir) => read_in(dir, &path),
            None => {
                Err("a workflow that is one file has no directory for `file` to read".to_owned())
            }
        };
        read.map_err(|reason| Exception::throw_message(&ctx, &reason))
    })?;
    globals.set("file", file)?;
    let args = match pass.args {
        Some(args) => ctx.json_parse(args.to_string())?,
        None => JsValue::new_undefined(ctx.clone()),
    };
    globals.set("args", args)
}

fn block<'js, B: Block>(ctx: &Ctx<'js>, state: &Rc<RefCell<State>>) -> rquickjs::Result<()> {
    let state = Rc::clone(state);
    let function = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, object: Opt<JsValue<'js>>| call::<B>(&ctx, &state, object.0),
    )?;
    ctx.globals().set(B::NAME.as_str(), function)
}

/// A call of block `B`: a pending promise numbered as the next call, or one already rejected
/// with `invalid` for an object the block's parser refuses.
fn call<'js, B: Block>(
    ctx: &Ctx<'js>,
    state: &RefCell<State>,
    object: Option<JsValue<'js>>,
) -> rquickjs::Result<Promise<'js>> {
    let (promise, resolve, reject) = ctx.promise()?;
    let object = match object {
        Some(object) => json_of(ctx, object),
        None => Ok(Value::Null),
    };
    let refusal = match &object {
        Err(reason) => Some(reason.clone()),
        Ok(object) => parse::<B>(object.clone(), Caller::Script)
            .err()
            .or_else(|| {
                (B::NAME == BlockName::Workflow).then(|| {
                "a workflow called from a script runs in its caller's run, which is not built yet"
                    .to_owned()
            })
            }),
    };
    let (Ok(object), None) = (object, &refusal) else {
        let failure = Failure {
            block: Some(B::NAME),
            ..pass_failure(Cause::Invalid, refusal.unwrap_or_default(), None)
        };
        reject.call::<_, ()>((failure_error(ctx, state, &failure)?,))?;
        return Ok(promise);
    };
    let digest = digest(B::NAME, &object);
    let mut state = state.borrow_mut();
    let number = state.calls.len() as u64 + 1;
    if state.mismatch.is_none()
        && let Some((logged, logged_digest)) = state.expected.get(&number)
        && (*logged != B::NAME || *logged_digest != digest)
    {
        let reason = if *logged == B::NAME {
            format!(
                "call {number} passes `{}` another object than the log's",
                B::NAME.as_str()
            )
        } else {
            format!(
                "call {number} is `{}` here and `{}` in the log",
                B::NAME.as_str(),
                logged.as_str()
            )
        };
        state.mismatch = Some(pass_failure(Cause::Replay, reason, Some(number)));
    }
    let settlers = (
        Persistent::save(ctx, resolve),
        Persistent::save(ctx, reject),
    );
    state.open.insert(number, settlers);
    state.calls.push(Call {
        number,
        block: B::NAME,
        object,
        digest,
    });
    Ok(promise)
}

/// A failure as the `Error` a script catches, with every field of it its own.
fn failure_error<'js>(
    ctx: &Ctx<'js>,
    state: &RefCell<State>,
    failure: &Failure,
) -> rquickjs::Result<JsValue<'js>> {
    let fail = state
        .borrow()
        .fail
        .clone()
        .ok_or(rquickjs::Error::Unknown)?;
    let fields = serde_json::to_string(failure).map_err(|_| rquickjs::Error::Unknown)?;
    fail.restore(ctx)?.call((ctx.json_parse(fields)?,))
}

/// A script value as JSON, or why it is not JSON.
fn json_of<'js>(ctx: &Ctx<'js>, value: JsValue<'js>) -> Result<Value, String> {
    let text = ctx
        .json_stringify(value)
        .and_then(|text| text.map(|text| text.to_string()).transpose())
        .catch(ctx)
        .map_err(|error| format!("the value is not JSON: {error}"))?;
    text.map_or(Ok(Value::Null), |text| {
        serde_json::from_str(&text).map_err(|error| format!("the value is not JSON: {error}"))
    })
}

/// The line of the first stack frame in the script `name`.
fn line_of(stack: &str, name: &str) -> Option<usize> {
    let marker = format!("{name}:");
    stack.match_indices(&marker).find_map(|(at, _)| {
        let digits: String = stack[at + marker.len()..]
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        digits.parse().ok()
    })
}

/// The text of `path` inside `dir`, or why it is refused.
fn read_in(dir: &Path, path: &str) -> Result<String, String> {
    let leaves = || {
        format!(
            "`file('{path}')` leaves the workflow's directory: name a file inside it, such as `prompts/fix.md`"
        )
    };
    let relative = Path::new(path);
    if path.is_empty()
        || !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
    {
        return Err(leaves());
    }
    let unread = |error: std::io::Error| format!("`file('{path}')`: {error}");
    let root: PathBuf = dir.canonicalize().map_err(unread)?;
    let full = root.join(relative).canonicalize().map_err(unread)?;
    if !full.starts_with(&root) {
        return Err(leaves());
    }
    std::fs::read_to_string(full).map_err(unread)
}

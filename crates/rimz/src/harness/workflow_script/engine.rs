//! The sandboxed QuickJS context one pass runs in, and the pass: the body runs until nothing more
//! can run, then each line of the log settles its call's promise and the body runs again.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
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
use super::{Call, Caps, End, Log, Mark, Outcome, Pass, Progress, Replay, pass_failure};
use crate::harness::block::{
    Agent, Block, BlockName, Caller, Cause, Failure, Now, Schedule, Shell, Team, Watch, Workflow,
    digest, ms, parse, reads,
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
    /// The title of the script's last `phase`: one allocation that its mark and every call made
    /// under it share, so the mark's charge against the heap's cap covers them all.
    phase: Option<Arc<str>>,
    marks: Vec<Mark>,
    /// How many lines the log holds, and how many of them the pass has handed back.
    lines: usize,
    used: usize,
}

impl State {
    /// Keep `progress` as a mark of the pass. The pass keeps it outside the engine's heap, so its
    /// bytes count against the heap's cap.
    fn mark(&mut self, heap: &Heap, progress: Progress) -> rquickjs::Result<()> {
        let text: &str = match &progress {
            Progress::Phase(title) => title,
            Progress::Log(line) => line,
        };
        if !heap.hold(std::mem::size_of::<Mark>() + text.len()) {
            return Err(rquickjs::Error::Allocation);
        }
        self.marks.push(Mark {
            after: self.calls.len() as u64,
            replayed: self.used < self.lines,
            progress,
        });
        Ok(())
    }

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
    let mut state = State {
        lines: match pass.log {
            Log::Recorded(lines) => lines.len(),
            Log::Answers(answers) => answers.len(),
        },
        ..State::default()
    };
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
    let (calls, marks, used, open, held) = {
        let mut state = state.borrow_mut();
        let open = state.open.keys().copied().collect::<Vec<_>>();
        let held = (
            std::mem::take(&mut state.unhandled),
            std::mem::take(&mut state.open),
            state.fail.take(),
        );
        let marks = std::mem::take(&mut state.marks);
        (
            std::mem::take(&mut state.calls),
            marks,
            state.used,
            open,
            held,
        )
    };
    drop(held);
    let end = replayed?;
    Ok(Replay {
        meta: loaded.meta,
        calls,
        open,
        used,
        marks,
        end,
    })
}

struct Engine<'a> {
    context: &'a Context,
    runtime: &'a Runtime,
    state: &'a Rc<RefCell<State>>,
    interrupted: &'a Cell<bool>,
    heap: &'a Rc<Heap>,
    caps: Caps,
    name: &'a str,
}

impl Engine<'_> {
    fn replay(&self, pass: &Pass<'_>, body: &str) -> Result<End, String> {
        let body = match self.start(pass, body)? {
            Ok(body) => body,
            Err(end) => return Ok(end),
        };
        if let Some(end) = self.pump() {
            return Ok(end);
        }
        let lines: Vec<(u64, &Outcome)> = match pass.log {
            Log::Recorded(lines) => lines
                .iter()
                .map(|line| (line.number, &line.outcome))
                .collect(),
            Log::Answers(answers) => (1..).zip(answers).collect(),
        };
        let recorded = matches!(pass.log, Log::Recorded(_));
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
            self.state.borrow_mut().used += 1;
            if let Some(end) = self
                .settle(number, block, outcome, settlers)
                .or_else(|| self.pump())
            {
                return Ok(end);
            }
        }
        Ok(self.end(body))
    }

    /// Build the globals, run the prelude, and start the body: the body's promise, or how the
    /// pass ended before it had one. An error is a body that does not compile.
    fn start(
        &self,
        pass: &Pass<'_>,
        body: &str,
    ) -> Result<Result<Persistent<Promise<'static>>, End>, String> {
        self.context.with(|ctx| {
            if let Err(error) = install(&ctx, self.state, self.heap, pass).catch(&ctx) {
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
                "line {line}: a workflow script has no imports: its globals are the blocks, `pipeline`, `parallel`, `cases`, `file`, `ms`, `reads`, `phase`, `log`, and `args`"
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

/// The script's globals: the seven blocks, `file`, `ms`, `reads`, `phase`, `log`, the prelude's,
/// which it builds on those, and `args`.
fn install<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    pass: &Pass<'_>,
) -> rquickjs::Result<()> {
    block::<Agent>(ctx, state)?;
    block::<Team>(ctx, state)?;
    block::<Shell>(ctx, state)?;
    block::<Watch>(ctx, state)?;
    block::<Workflow>(ctx, state)?;
    block::<Schedule>(ctx, state)?;
    block::<Now>(ctx, state)?;
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
    globals.set(
        "ms",
        Function::new(ctx.clone(), |ctx: Ctx<'js>, duration: String| {
            ms(&duration)
                .map_err(|reason| Exception::throw_message(&ctx, &format!("`ms`: {reason}")))
        })?,
    )?;
    globals.set(
        "reads",
        Function::new(
            ctx.clone(),
            |ctx: Ctx<'js>, when: String, readings: JsValue<'js>| truth(&ctx, &when, readings),
        )?,
    )?;
    globals.set("phase", {
        let (state, heap) = (Rc::clone(state), Rc::clone(heap));
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, title: Opt<JsValue<'js>>| {
                let title = text(&ctx, title, "`phase` takes its title as text")?;
                let mut state = state.borrow_mut();
                if state.phase.as_deref() == Some(title.as_str()) {
                    return Ok(());
                }
                let title = Arc::<str>::from(title);
                state.phase = Some(Arc::clone(&title));
                state.mark(&heap, Progress::Phase(title))
            },
        )?
    })?;
    globals.set("log", {
        let (state, heap) = (Rc::clone(state), Rc::clone(heap));
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, line: Opt<JsValue<'js>>| {
                let line = text(&ctx, line, "`log` takes its line as text")?;
                state.borrow_mut().mark(&heap, Progress::Log(line))
            },
        )?
    })?;
    let fail = ctx.eval_with_options::<Function, _>(PRELUDE, options(PRELUDE_NAME))?;
    state.borrow_mut().fail = Some(Persistent::save(ctx, fail));
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
        Some(_) if B::BARE => Err(format!(
            "a `{name}` call takes no object: call `{name}()`",
            name = B::NAME.as_str()
        )),
        Some(object) => json_of(ctx, object),
        None if B::BARE => Ok(Value::Object(serde_json::Map::new())),
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
    let phase = state.phase.clone();
    state.calls.push(Call {
        number,
        block: B::NAME,
        object,
        digest,
        phase,
    });
    Ok(promise)
}

/// The text a script passed, or `refusal` thrown for anything else.
fn text<'js>(ctx: &Ctx<'js>, value: Opt<JsValue<'js>>, refusal: &str) -> rquickjs::Result<String> {
    match value.0.as_ref().and_then(JsValue::as_string) {
        Some(text) => text.to_string(),
        None => Err(Exception::throw_message(ctx, refusal)),
    }
}

/// A script's `reads`: the truth of `when` over `readings`, `null` for unknown.
fn truth<'js>(
    ctx: &Ctx<'js>,
    when: &str,
    readings: JsValue<'js>,
) -> rquickjs::Result<JsValue<'js>> {
    let refused = |reason: String| Exception::throw_message(ctx, &format!("`reads`: {reason}"));
    let readings = json_of(ctx, readings)
        .and_then(|readings| {
            serde_json::from_value(readings).map_err(|error| {
                format!("the readings are not an object of strings and nulls: {error}")
            })
        })
        .map_err(refused)?;
    Ok(match reads(when, &readings).map_err(refused)? {
        Some(truth) => JsValue::new_bool(ctx.clone(), truth),
        None => JsValue::new_null(ctx.clone()),
    })
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

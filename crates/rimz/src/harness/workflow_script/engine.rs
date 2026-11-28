//! The sandboxed QuickJS context one pass runs in, and the pass: the body runs until nothing more
//! can run, then each line of the log settles its call's promise and the body runs again.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use rquickjs::context::{EvalOptions, intrinsic};
use rquickjs::function::Opt;
use rquickjs::promise::PromiseState;
use rquickjs::{
    CatchResultExt as _, CaughtError, Context, Ctx, Exception, Function, Object, Persistent,
    Promise, Runtime, Value as JsValue,
};
use serde_json::Value;

use super::heap::{Capped, Heap};
use super::load::Loaded;
use super::{
    Call, Called, Calling, Caps, End, Log, Mark, Outcome, Pass, Progress, Reader, Replay,
    pass_failure,
};
use crate::harness::block::{
    Agent, Block, BlockName, Caller, Cause, Failure, Now, Schedule, Shell, Team, Watch, Workflow,
    WorkflowSource, digest, ms, parse, reads,
};

const PRELUDE: &str = include_str!("prelude.js");

/// The name the prelude's own frames carry, apart from the script's.
const PRELUDE_NAME: &str = "<rimz>";

/// Why a body that closes the function it runs in does not run.
const UNBALANCED: &str = "the body's braces do not balance: it closes the function it runs in";

/// How many `workflow` calls deep a call may run, the outermost one counted.
const DEPTH: usize = 8;

type Settlers = (Persistent<Function<'static>>, Persistent<Function<'static>>);

/// What the block functions and the rejection tracker share with the pass.
#[derive(Default)]
struct State {
    calls: Vec<Call>,
    /// The calls no line has answered yet, with their promise's settle functions.
    open: BTreeMap<u64, Settlers>,
    /// The calls taken out of `open` when a `workflow` call they ran under settled: a line for
    /// one is the run's, answered before the pass saw the call settle, and settles nothing.
    dropped: BTreeSet<u64>,
    /// A recorded log's block and digest per call number.
    expected: BTreeMap<u64, (BlockName, String)>,
    /// The first failure of the pass itself: a call that differs from the log, or the call past
    /// the step cap.
    halt: Option<Failure>,
    /// Rejected promises nothing handles yet, with their reasons, in the order they were rejected.
    unhandled: Vec<(Persistent<JsValue<'static>>, Persistent<JsValue<'static>>)>,
    /// The prelude's maker of a failure's `Error`, its maker of a body's `cases`, and its link
    /// from a called body's promise to what settles the call.
    fail: Option<Persistent<Function<'static>>>,
    cases_of: Option<Persistent<Function<'static>>>,
    link: Option<Persistent<Function<'static>>>,
    /// The title of each body's last `phase`, the run's own script's under none: one allocation
    /// that its mark and every call made under it share, so the mark's charge against the heap's
    /// cap covers them all.
    phases: BTreeMap<Option<u64>, Arc<str>>,
    marks: Vec<Mark>,
    /// How many of the log's lines the pass has handed back, and how many the run's last pass
    /// covered.
    used: usize,
    covered: Option<usize>,
    steps: Option<u64>,
    reader: Option<Rc<Reader>>,
    /// The file names of the called scripts, which an error thrown in one carries.
    names: Vec<String>,
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
            replayed: self.covered.is_some_and(|covered| self.used <= covered),
            progress,
        });
        Ok(())
    }

    /// Whether call `number` is a `workflow` call, which no line but its failure answers.
    fn is_workflow(&self, number: u64) -> bool {
        self.call(number)
            .is_some_and(|call| call.block == BlockName::Workflow)
    }

    fn call(&self, number: u64) -> Option<&Call> {
        let index = usize::try_from(number.checked_sub(1)?).ok()?;
        self.calls.get(index)
    }

    /// Take every call open under `workflow` call `number`, at any depth, out of the open
    /// calls into the dropped ones, so none is ever settled.
    fn drop_under(&mut self, number: u64) {
        let mut under = BTreeSet::from([number]);
        let after = usize::try_from(number).unwrap_or(usize::MAX);
        for call in self.calls.iter().skip(after) {
            if call.parent.is_some_and(|parent| under.contains(&parent)) {
                under.insert(call.number);
                if self.open.remove(&call.number).is_some() {
                    self.dropped.insert(call.number);
                }
            }
        }
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
        covered: pass.covered,
        steps: pass.steps,
        reader: pass.reader.clone(),
        ..State::default()
    };
    if let Log::Recorded(lines) = pass.log {
        for line in lines {
            let twice = state
                .expected
                .insert(line.number, (line.block, line.digest.clone()))
                .is_some();
            if twice && state.halt.is_none() {
                let reason = format!("the log answers call {} twice", line.number);
                state.halt = Some(pass_failure(Cause::Replay, reason, Some(line.number)));
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
            (state.fail.take(), state.cases_of.take(), state.link.take()),
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
            if self.settled(&body) || self.state.borrow().halt.is_some() {
                break;
            }
            let taken = {
                let mut state = self.state.borrow_mut();
                let dropped = state.dropped.remove(&number);
                let block = state.call(number).map(|call| call.block);
                match (state.open.remove(&number), block) {
                    (settlers, Some(BlockName::Workflow))
                        if (settlers.is_some() || dropped)
                            && matches!(outcome, Outcome::Result(_)) =>
                    {
                        let reason = format!(
                            "the log answers call {number}, a workflow, with a result: a workflow's body gives its result"
                        );
                        state.halt = Some(pass_failure(Cause::Replay, reason, Some(number)));
                        None
                    }
                    // Its call was made as the line's block and digest, or the pass halted there.
                    _ if dropped => {
                        state.used += 1;
                        continue;
                    }
                    (Some(settlers), Some(block)) => {
                        if block == BlockName::Workflow {
                            state.drop_under(number);
                        }
                        Some((settlers, block))
                    }
                    // A recorded line answers a call the run made, so one the replay has not
                    // made is a script that differs; an answers file just runs out of calls.
                    _ if !recorded && state.open.is_empty() => None,
                    (_, made) => {
                        let reason = match made {
                            Some(BlockName::Workflow) => format!(
                                "the log answers call {number}, a workflow whose body has ended"
                            ),
                            Some(_) => format!("the log answers call {number} twice"),
                            None => format!(
                                "the log answers call {number}, which the script has not made"
                            ),
                        };
                        state.halt = Some(pass_failure(Cause::Replay, reason, Some(number)));
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
                    None => Err(UNBALANCED.to_owned()),
                },
                Err(CaughtError::Exception(error))
                    if error
                        .get::<_, String>("name")
                        .is_ok_and(|name| name == "SyntaxError") =>
                {
                    Err(syntax(self.name, body, &error))
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
        if let Some(failure) = self.state.borrow_mut().halt.take() {
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
                PromiseState::Pending if self.waits_on_a_step() => End::Pending,
                PromiseState::Pending => self.unhandled(&ctx).unwrap_or_else(|| {
                    let reason = "the script awaits something no call will settle".to_owned();
                    End::Failed(pass_failure(Cause::Stuck, reason, None))
                }),
            }
        })
    }

    /// Whether a call other than a `workflow` call is open: one a line may still answer.
    fn waits_on_a_step(&self) -> bool {
        let state = self.state.borrow();
        state.open.keys().any(|number| !state.is_workflow(*number))
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
        // A script's own `stack` getter may call a block function, which borrows the state.
        let stack = error.stack();
        let state = self.state.borrow();
        let names = std::iter::once(self.name).chain(state.names.iter().map(String::as_str));
        match stack.and_then(|stack| first_frame(&stack, names)) {
            Some((name, line)) => format!("{text} ({name}:{line})"),
            None => text,
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

/// A body that does not compile, at its line in the script `name`.
fn syntax(name: &str, body: &str, error: &Exception<'_>) -> String {
    let message = error.message().unwrap_or_default();
    let Some(line) = error
        .stack()
        .and_then(|stack| first_frame(&stack, std::iter::once(name)))
        .map(|(_, line)| line)
    else {
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

/// The script's globals: the seven blocks, `file`, `ms`, `reads`, `phase`, `log`, the prelude's,
/// which it builds on those, and `args`.
fn install<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    pass: &Pass<'_>,
) -> rquickjs::Result<()> {
    let globals = ctx.globals();
    for (name, function) in blocks(ctx, state, heap, None)? {
        globals.set(name, function)?;
    }
    globals.set("file", file(ctx, pass.dir.map(Path::to_path_buf))?)?;
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
    globals.set("phase", phase(ctx, state, heap, None)?)?;
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
    let made = ctx.eval_with_options::<Object, _>(PRELUDE, options(PRELUDE_NAME))?;
    {
        let mut state = state.borrow_mut();
        state.fail = Some(Persistent::save(ctx, made.get::<_, Function>("fail")?));
        state.cases_of = Some(Persistent::save(ctx, made.get::<_, Function>("casesOf")?));
        state.link = Some(Persistent::save(ctx, made.get::<_, Function>("link")?));
    }
    globals.set("args", json_value(ctx, pass.args)?)
}

/// The seven blocks, by name, whose calls carry `parent`.
fn blocks<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    parent: Option<u64>,
) -> rquickjs::Result<[(&'static str, Function<'js>); 7]> {
    Ok([
        block::<Agent>(ctx, state, heap, parent)?,
        block::<Team>(ctx, state, heap, parent)?,
        block::<Shell>(ctx, state, heap, parent)?,
        block::<Watch>(ctx, state, heap, parent)?,
        block::<Workflow>(ctx, state, heap, parent)?,
        block::<Schedule>(ctx, state, heap, parent)?,
        block::<Now>(ctx, state, heap, parent)?,
    ])
}

fn block<'js, B: Block>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    parent: Option<u64>,
) -> rquickjs::Result<(&'static str, Function<'js>)> {
    let (state, heap) = (Rc::clone(state), Rc::clone(heap));
    let function = Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, object: Opt<JsValue<'js>>| {
            call::<B>(&ctx, &state, &heap, object.0, parent)
        },
    )?;
    Ok((B::NAME.as_str(), function))
}

/// A body's `file`, which reads inside `dir`.
fn file<'js>(ctx: &Ctx<'js>, dir: Option<PathBuf>) -> rquickjs::Result<Function<'js>> {
    Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
        let read = match &dir {
            Some(dir) => read_in(dir, &path),
            None => {
                Err("a workflow that is one file has no directory for `file` to read".to_owned())
            }
        };
        read.map_err(|reason| Exception::throw_message(&ctx, &reason))
    })
}

/// A body's `phase`, which titles the calls the body under `parent` makes.
fn phase<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    parent: Option<u64>,
) -> rquickjs::Result<Function<'js>> {
    let (state, heap) = (Rc::clone(state), Rc::clone(heap));
    Function::new(
        ctx.clone(),
        move |ctx: Ctx<'js>, title: Opt<JsValue<'js>>| {
            let title = text(&ctx, title, "`phase` takes its title as text")?;
            let mut state = state.borrow_mut();
            if state.phases.get(&parent).map(|title| &**title) == Some(title.as_str()) {
                return Ok(());
            }
            let title = Arc::<str>::from(title);
            state.phases.insert(parent, Arc::clone(&title));
            state.mark(&heap, Progress::Phase(title))
        },
    )
}

/// `args` as a script value, `undefined` for none.
fn json_value<'js>(ctx: &Ctx<'js>, value: Option<&Value>) -> rquickjs::Result<JsValue<'js>> {
    match value {
        Some(value) => ctx.json_parse(value.to_string()),
        None => Ok(JsValue::new_undefined(ctx.clone())),
    }
}

/// A call of block `B` by the body under `parent`: a pending promise numbered as the next call,
/// one already rejected with `invalid` for an object the block's parser refuses, or one that
/// never settles and takes no number for a call past the step cap or under a `workflow` call
/// that has settled.
fn call<'js, B: Block>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    object: Option<JsValue<'js>>,
    parent: Option<u64>,
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
                (B::NAME == BlockName::Workflow && state.borrow().reader.is_none()).then(|| {
                    "this walk of the script does not follow a call into another workflow"
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
    let mut held = state.borrow_mut();
    if parent.is_some_and(|parent| !held.open.contains_key(&parent)) {
        return Ok(promise);
    }
    let number = held.calls.len() as u64 + 1;
    if let Some(cap) = held.steps
        && number > cap
    {
        if held.halt.is_none() {
            let reason =
                format!("the script would make call {number}, past the run's step cap of {cap}");
            held.halt = Some(pass_failure(Cause::Steps, reason, Some(number)));
        }
        return Ok(promise);
    }
    if held.halt.is_none()
        && let Some((logged, logged_digest)) = held.expected.get(&number)
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
        held.halt = Some(pass_failure(Cause::Replay, reason, Some(number)));
    }
    let settlers = (
        Persistent::save(ctx, resolve),
        Persistent::save(ctx, reject),
    );
    held.open.insert(number, settlers);
    let phase = held.phases.get(&parent).cloned();
    held.calls.push(Call {
        number,
        block: B::NAME,
        object,
        digest,
        parent,
        phase,
    });
    drop(held);
    if B::NAME == BlockName::Workflow {
        start_called(ctx, state, heap, number)?;
    }
    Ok(promise)
}

/// Start the body of `workflow` call `number` at once, its calls numbered in the caller's run,
/// or fail the call `invalid` at its number when its script cannot be read or it runs too deep.
fn start_called<'js>(
    ctx: &Ctx<'js>,
    state: &Rc<RefCell<State>>,
    heap: &Rc<Heap>,
    number: u64,
) -> rquickjs::Result<()> {
    let (workflow, parent, depth, reader) = {
        let held = state.borrow();
        let call = held.call(number).ok_or(rquickjs::Error::Unknown)?;
        let workflow = parse::<Workflow>(call.object.clone(), Caller::Script)
            .map_err(|_| rquickjs::Error::Unknown)?;
        let depth = std::iter::successors(call.parent, |parent| {
            held.call(*parent).and_then(|call| call.parent)
        })
        .count()
            + 1;
        (workflow, call.parent, depth, held.reader.clone())
    };
    let refused = |reason: String| {
        let failure = Failure {
            block: Some(BlockName::Workflow),
            ..pass_failure(Cause::Invalid, reason, Some(number))
        };
        let error = failure_error(ctx, state, &failure)?;
        settle_called(ctx, state, number, Err(error))
    };
    if depth > DEPTH {
        return refused(format!(
            "call {number} would run a workflow {depth} calls deep, past the bound of {DEPTH}"
        ));
    }
    let read = reader.ok_or(rquickjs::Error::Unknown)?(&Calling {
        number,
        parent,
        source: workflow.source,
        value: &workflow.value,
    });
    let Called { name, text, dir } = match read {
        Ok(called) => called,
        Err(reason) => return refused(reason),
    };
    let body = match super::load::body(&text, workflow.source == WorkflowSource::Script) {
        Ok(body) => body,
        Err(reason) => return refused(format!("{name}: {reason}")),
    };
    let source =
        format!("(function ({{ {BINDINGS} }}) {{ return async function () {{{body}\n}} }})");
    let made = match ctx.eval_with_options::<JsValue, _>(source, options(&name)) {
        Ok(made) => made,
        Err(rquickjs::Error::Exception) => {
            let thrown = ctx.catch();
            let error = thrown
                .as_object()
                .and_then(|object| Exception::from_object(object.clone()))
                .filter(|error| {
                    error
                        .get::<_, String>("name")
                        .is_ok_and(|name| name == "SyntaxError")
                });
            match error {
                Some(error) => return refused(format!("{name}: {}", syntax(&name, &body, &error))),
                None => return Err(ctx.throw(thrown)),
            }
        }
        Err(error) => return Err(error),
    };
    let unbalanced = format!("{name}: {UNBALANCED}");
    let Some(made) = made.into_function() else {
        return refused(unbalanced);
    };
    state.borrow_mut().names.push(name);
    let bindings = Object::new(ctx.clone())?;
    for (block, function) in blocks(ctx, state, heap, Some(number))? {
        bindings.set(block, function)?;
    }
    let cases_of = state
        .borrow()
        .cases_of
        .clone()
        .ok_or(rquickjs::Error::Unknown)?;
    let cases = cases_of.restore(ctx)?.call::<_, Function>((
        bindings.get::<_, JsValue>("watch")?,
        bindings.get::<_, JsValue>("now")?,
    ))?;
    bindings.set("cases", cases)?;
    bindings.set("file", file(ctx, dir)?)?;
    bindings.set("phase", phase(ctx, state, heap, Some(number))?)?;
    bindings.set("args", json_value(ctx, workflow.args.as_ref())?)?;
    let Some(run) = made.call::<_, JsValue>((bindings,))?.into_function() else {
        return refused(unbalanced);
    };
    let body = run.call::<_, JsValue>(())?;
    let link = state
        .borrow()
        .link
        .clone()
        .ok_or(rquickjs::Error::Unknown)?;
    let settled = {
        let state = Rc::clone(state);
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, value: JsValue<'js>| {
            settle_called(&ctx, &state, number, Ok(value))
        })?
    };
    let failed = {
        let state = Rc::clone(state);
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, value: JsValue<'js>| {
            settle_called(&ctx, &state, number, Err(value))
        })?
    };
    link.restore(ctx)?.call::<_, ()>((body, settled, failed))
}

/// The names a called body binds as its own, which shadow the run's own script's globals.
const BINDINGS: &str =
    "agent, team, shell, watch, workflow, schedule, now, cases, file, phase, args";

/// Settle `workflow` call `number` with what its body gave, unless it is settled already,
/// and take every call still open under it out of the open calls.
fn settle_called<'js>(
    ctx: &Ctx<'js>,
    state: &RefCell<State>,
    number: u64,
    outcome: Result<JsValue<'js>, JsValue<'js>>,
) -> rquickjs::Result<()> {
    let (resolve, reject) = {
        let mut held = state.borrow_mut();
        let Some(settlers) = held.open.remove(&number) else {
            return Ok(());
        };
        held.drop_under(number);
        settlers
    };
    match outcome {
        Ok(value) => resolve.restore(ctx)?.call((value,)),
        Err(value) => reject.restore(ctx)?.call((value,)),
    }
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

/// The script and line of the first stack frame in one of the scripts `names`.
fn first_frame<'n>(stack: &str, names: impl Iterator<Item = &'n str>) -> Option<(&'n str, usize)> {
    names
        .filter_map(|name| {
            let marker = format!("{name}:");
            stack.match_indices(&marker).find_map(|(at, _)| {
                let digits: String = stack[at + marker.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                Some((at, name, digits.parse().ok()?))
            })
        })
        .min_by_key(|(at, _, _)| *at)
        .map(|(_, name, line)| (name, line))
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

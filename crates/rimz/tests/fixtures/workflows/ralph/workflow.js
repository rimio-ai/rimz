// Ralph: one goal built by fresh agents, one plan item at a time, in one worktree, from a
// goal directory outside the repo that the ralph agent wrote with the user.
//
// DESIGN SKETCH, not runnable: no RimZ loads this format yet. It re-implements
// ~/.agents/factory/workflows/ralph/ in the form ../../design.md defines. Today the loop over
// milestones and items, the tries per item, and the run's arguments live in checks/ralph.py
// (pick, question, swept, ended, fault) and its ledger. Here they are the loops and variables
// of this file. Start it with the goal directory as args.goal, and with a launch cap for the
// goal.
//
// ralph.py keeps what only it can know, the facts about the work, behind the entry points this
// sketch assumes. Each reads the goal from args and the rest from input, in its context file,
// and prints one JSON answer:
//   todo     unplanned (no valid plan, or specs changed since the plan), ready with the
//            milestones and items still unbuilt, or built when none is left.
//   start    before a milestone's first item: pass when the done check is green, replan when
//            the builds reported problems the plan has not seen.
//   gate     today's gate, whole: the claim, its item, a clean tree, a new commit, the done
//            check, and the test count. landed, retry with what to fix first, or triage (a
//            blocked claim or a test count that fell). It reads try and tries from input: a
//            claim blocked on the last attempt answers retry, which sets the item aside.
//   accept   settles an item a judge accepted, which only a commit the gate passed allows:
//            landed, or retry.
//   aside    records a given-up item in the ledger and moves its commits off the branch.
//   changed  some when the milestone's branch holds commits to review and ship, none when not.
//   pr       open when the milestone has a pull request, none when it needs one, off when the
//            goal opens none.
//   trouble  what keeps the PR out: sweep, answered (every change request already went to a
//            sweep), or report after two sweeps of one kind since the PR last left the queue.
//   advance  after a merge: records it and puts the checkout on a fresh branch.
// Every one is safe to run twice. gate, accept, aside, and advance write the ledger or the
// branch, so each asks first whether its item or its milestone is already recorded. Today's
// gate does not, and has to learn it.
//
// Different from today's ralph:
// - A worker that is blocked says so in its answer, and a judge takes its question. A question
//   typed into a worker's pane waits for the user, where today it reaches the judge.
// - A third review reads the second round of fixes, where today the milestone ships with them
//   unread. A plan attempt's question has its own judge. A question on an item's fourth
//   attempt reaches the judge, where today the item is given up unasked. What the next plan
//   must follow (an audit's gaps, a replan, a ruling) is a variable here, where today todo
//   keeps it.
// - The caps on one step's launches (200 builds, 40 fixes, 16 plans, 40 ships, 40 sweeps) are
//   gone: the loops of this file count, and the run's launch cap bounds what they add up to.
// - A run that hits its timeout or its launch cap ends failed, with no report: whoever
//   resumes it gives it more, and todo says what is left.
//
// Rules of today's ralph this sketch leaves out: the spend budget, which spans every run of a
// goal and so stays a ledger of ralph.py, and sign syncing into the goal directory.
export const meta = {
  name: 'ralph',
  description: 'Builds one goal with fresh agents, one plan item at a time, in one worktree',
}

// An error this file throws to stop the loop itself: a judge's ruling, or a count that ran
// out. The report says so. Any other failure opens the ralph agent.
class Stop extends Error {}

// A brief: a prompt file, and as JSON under it the run's arguments and the values it needs.
const brief = (path, data) =>
  file(path + '.md') + '\n\n' + JSON.stringify({ args, ...data }, null, 2)
// What an agent is told of a failure: a block's failure carries its own fields (a command's
// log path stands for its output), and any other error its message.
const told = failure => ({
  ...failure, stdout: undefined, reason: failure.reason ?? failure.message })

// A fact about the work, from ralph.py.
const ralph = async (entry, input, timeout = '10m') => JSON.parse(
  (await shell({ command: 'uv run --script "$RIMZ_WORKFLOW_FILES"/checks/ralph.py ' + entry, input, timeout })).stdout)

// A judge: an agent that answers one of the words it is given. Its prompt sits under checks/.
const judge = (name, words, data, timeout = '2h') => agent({
  profile: 'judge', stay: false, timeout, prompt: brief('checks/' + name, data),
  schema: { type: 'object', required: ['outcome'], properties: { outcome: { enum: words } } } })

// What a worker hands in: done, or blocked with its question. A timed-out or dead attempt is
// handed on as its failure, and is checked like any other.
const CLAIM = { type: 'object', required: ['outcome'],
                properties: { outcome: { enum: ['done', 'blocked'] },
                              question: { type: 'string' } } }
const worker = (name, timeout, data, profile = 'opus') =>
  agent({ profile, stay: false, timeout, schema: CLAIM, prompt: brief('prompts/' + name, data) })
    .catch(failure => failure)

// Four attempts at one piece of work: a plan item, or a review's fixes. Each attempt reads why
// the last one was sent back. A judge rules where the gate cannot: it answers a worker's
// question, accepts or skips the work, calls a replan, or stops the loop. The judge is still
// asked after the fourth attempt, so it can accept a green commit whose test count fell.
// Answers {} when the work landed, { aside } when it was given up, { replan } for a new plan.
async function build(input) {
  let why
  for (let n = 1; n <= 4; n++) {
    const attempt = await worker('build', '4h', { ...input, why })
    // A question goes straight to the judge.
    const gate = attempt.outcome === 'blocked'
      ? { outcome: 'triage', question: attempt.question }
      : await ralph('gate', { ...input, attempt, try: n, tries: 4 }, '1d')
    if (gate.outcome === 'landed') return {}
    why = gate
    if (gate.outcome !== 'triage') continue

    const ruling = await judge('triage', ['build', 'accept', 'skip', 'plan', 'report'],
                               { ...input, gate })
    if (ruling.outcome === 'report') throw new Stop('a judge stopped the loop')
    if (ruling.outcome === 'plan') return { replan: ruling }
    if (ruling.outcome === 'skip') break
    why = ruling
    if (ruling.outcome === 'accept') {
      const settled = await ralph('accept', input)
      if (settled.outcome === 'landed') return {}
      why = settled
    }
  }
  await ralph('aside', input)
  return { aside: true }
}

// A judge of the other family reviews the built milestone's diff and writes the blocking
// fixes to its findings file, which one fix build per round works through. Three reviews, so
// two rounds of fixes: no fix starts after the third review, since no review would read it,
// and the milestone ships as it is. A fix that is given up ships the milestone too, as today.
async function review(m) {
  for (let n = 1; n <= 3; n++) {
    const found = await judge('review', ['clean', 'revise'], { milestone: m })
    if (found.outcome === 'clean' || n === 3) return {}
    const fixed = await build({ milestone: m, findings: found })
    if (fixed.replan || fixed.aside) return fixed
  }
}

// Four looks for a pull request, so three ships. Whatever a ship attempt hands in, the next
// look reads the forge. False when the goal opens no pull request.
async function shipped(m) {
  for (let look = 1; look <= 4; look++) {
    const pr = await ralph('pr', { milestone: m })
    if (pr.outcome === 'open') return true
    if (pr.outcome === 'off') return false
    if (look < 4) await worker('ship', '2h', { milestone: m })
  }
  throw new Stop('no pull request after three ship attempts')
}

// What keeps the pull request out, or its end, in this order: no PR CI runs on a conflicted
// head, and the review reads last, since it has no reading on Gitea. A queued PR waits, and so
// does a PR whose CI is pending before its review counts.
const PR = [
  { outcome: 'merged', when: 'pr=merged' },
  { outcome: 'closed', when: 'pr=closed' },
  { when: 'pr=open && pr.queue=queued' },
  { outcome: 'dequeued', when: 'pr=open && pr.queue=dequeued' },
  { outcome: 'conflicted', when: 'pr=open && pr.mergeable=conflicting' },
  { outcome: 'red', when: 'pr=open && ci=failed' },
  { when: 'ci=pending' },
  { outcome: 'changes', when: 'pr=open && pr.review=changes_requested' },
]

// The run never merges: it waits here for the user or another workflow to. trouble keeps the
// count of sweeps per kind and of change requests already answered. 700 looks cover seven
// days of fifteen-minute holds while a reviewer has not answered, and the run's launch cap
// bounds the launches.
async function merged(m) {
  for (let look = 1; look <= 700; look++) {
    const pr = await cases(PR, { timeout: '7d' })
    if (pr.outcome === 'merged') return
    if (pr.outcome === 'closed') throw new Stop('the pull request was closed')
    const trouble = await ralph('trouble', { milestone: m, watch: pr })
    if (trouble.outcome === 'report') {
      throw new Stop('the same trouble came back after two sweeps')
    }
    if (trouble.outcome === 'answered') await shell({ command: 'sleep 900' })
    else await worker('sweep', '3h', { milestone: m, watch: pr, trouble })
  }
  throw new Stop('seven hundred looks at one pull request')
}

// Answers { replan } when the plan must change before the work goes on.
async function milestone(m) {
  const start = await ralph('start', { milestone: m }, '1d')
  if (start.outcome === 'replan') return { replan: start }
  for (const item of m.items) {
    const built = await build({ milestone: m, item })
    if (built.replan) return built
  }
  // A milestone with nothing to review or ship is finished.
  if ((await ralph('changed', { milestone: m })).outcome === 'none') return {}
  const reviewed = await review(m)
  if (reviewed.replan) return reviewed
  // A goal that opens no pull request skips the watch and the advance.
  if (!await shipped(m)) return {}
  await merged(m)
  await ralph('advance', { milestone: m })
  return {}
}

// The planner. A planner that is blocked hands its question to a judge, whose ruling the next
// plan attempt follows. Answers what the next plan must still follow.
async function plan(brief) {
  const attempt = await worker('plan', '3h', { brief }, 'fable')
  if (attempt.outcome !== 'blocked') return undefined
  const ruling = await judge('triage', ['plan', 'report'], { brief, question: attempt.question })
  if (ruling.outcome === 'report') throw new Stop('the plan judge stopped the loop')
  return ruling
}

// todo reads the plan. The planner runs when there is something the plan must follow or todo
// answers unplanned, and the milestones still unbuilt run when it answers ready. Forty looks
// at the plan in one pass.
async function buildAll(brief) {
  planning: for (let look = 1; look <= 40; look++) {
    const todo = await ralph('todo')
    if (brief || todo.outcome === 'unplanned') { brief = await plan(brief); continue }
    if (todo.outcome === 'built') return
    for (const m of todo.milestones) {
      const went = await milestone(m)
      if (went.replan) { brief = went.replan; continue planning }
    }
  }
  throw new Stop('forty looks at the plan and the goal is not built')
}

// Checks the finished work against every spec's acceptance criteria. Gaps are what the next
// plan must follow. Three audits, so two replans: gaps the third audit still finds stop the
// run.
async function goal() {
  let gaps
  for (let n = 1; n <= 3; n++) {
    await buildAll(gaps)
    gaps = await judge('audit', ['complete', 'gaps'], undefined, '3h')
    if (gaps.outcome === 'complete') return
  }
  throw new Stop('the third audit still finds gaps')
}

// The report runs when the work completed and when this file stopped it (a judge, a count).
// Anything else (a broken goal directory, a check that died or answered no JSON) opens the
// ralph agent, which stays open beside the user. The run then ends failed, and whoever fixed
// the cause resumes it: todo says what is left.
let ending
try {
  await goal()
  ending = { outcome: 'complete' }
} catch (failure) {
  if (!(failure instanceof Stop)) {
    await agent({ profile: 'ralph', prompt: brief('prompts/break', { failure: told(failure) }) })
      .catch(() => undefined)
    throw failure
  }
  ending = { outcome: 'stopped', reason: failure.message }
}
await agent({ profile: 'opus', stay: false, timeout: '2h',
              prompt: brief('prompts/report', { ending }) })
if (ending.outcome === 'stopped') throw new Error(ending.reason)
return ending

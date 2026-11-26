// Steward: one task run over many units by fresh workers, one worker per unit, from a job
// directory outside the repo that the steward agent wrote.
//
// DESIGN SKETCH, not runnable: no RimZ loads this format yet. It re-implements
// ~/.agents/factory/workflows/steward/ in the form ../../design.md defines. Today a generator
// script copies the workflow once per job and lane with one work step per worker profile and
// the job's timeouts, and checks/steward.py (pick, ended, fault) hands out units and counts
// tries per unit. Here this one file takes the job as an argument, reads its lanes once, and
// works them side by side in one run, each lane's units in order. Start it with a launch cap
// for the job: five launches a unit at most (three workers, the answer judge, the review
// judge).
//
// steward.py keeps the facts about the work behind the entry points this sketch assumes.
// Each reads the job and the batch from args and the rest from input, in its context file,
// and prints one JSON answer:
//   lanes   the job's two timeouts and its lanes still due, each with its units still due in
//           order, and each unit with its kind, and its worker profile, or its team and the
//           team's worktree. Nothing when the job is halted.
//   gate    today's gate, whole: reads the unit's result, checks it against the repo, lands
//           it. landed, or retry with what to fix first.
//   fail    records a unit whose attempts are spent as failed, with the last reason.
//   settle  records a unit the answer judge skipped.
//   halt    records that a judge halted the job.
//   due     after a unit: review when its result or the failures that repeat call for the
//           review judge, none when not.
//   sign    appends the line the review judge wrote to the job's signs.md.
// Every one is safe to run twice: gate, fail, settle, halt, and sign ask the ledger first
// whether the unit is already recorded. They keep today's lock on the job directory around
// their commits and their ledger: the lanes share one worktree, and the engine serializes
// nothing between them.
//
// Different from today's steward:
// - A lane is no run of its own. It cannot be canceled or resumed apart from the others: a
//   resume of the run asks lanes what is still due, so a lane that finished is not redone.
// - A worker that is blocked says so in its answer, and the answer judge takes its question.
//   A question typed into a worker's pane waits for the user.
// - A team unit is a team block, where today an agent launches the team and waits for it.
// - How many lanes work at once is the run's parallel cap, where today it is eight.
// - A run that hits its timeout or its launch cap ends failed, with no report.
//
// Not checked against steward.py: that lanes can answer from the batch a restart names.
export const meta = {
  name: 'steward',
  description: 'Runs one task over the units of a job, lanes side by side, one worker per unit',
}

const steward = async (entry, input, timeout = '10m') => JSON.parse(
  (await shell({ command: 'uv run --script checks/steward.py ' + entry, input, timeout })).stdout)

// A brief: a prompt file, and as JSON under it the run's arguments and the values it needs.
const brief = (path, data) =>
  file(path + '.md') + '\n\n' + JSON.stringify({ args, ...data }, null, 2)
// What an agent is told of a failure: a block's failure carries its own fields (a command's
// log path stands for its output), and any other error its message.
const told = failure => ({
  ...failure, stdout: undefined, reason: failure.reason ?? failure.message })

// A judge: an agent that answers one of the words it is given. Its prompt sits under checks/.
const judge = (name, words, data) => agent({
  profile: 'judge', stay: false, timeout: '2h', prompt: brief('checks/' + name, data),
  schema: { type: 'object', required: ['outcome'], properties: { outcome: { enum: words } } } })

// What a worker hands in: done, or blocked with its question.
const CLAIM = { type: 'object', required: ['outcome'],
                properties: { outcome: { enum: ['done', 'blocked'] },
                              question: { type: 'string' } } }

// The kinds a job may name. A job outside them stops the run before any lane starts.
const KINDS = ['solo', 'team']

// One attempt. The worker profile, the team, and the timeouts come from the job. A dead or
// timed-out attempt goes to the gate like any other: its failure is what the gate is told.
const work = (job, unit, why) => (unit.kind === 'team'
  ? team({ team: unit.team, worktree: unit.worktree, timeout: job.teamTimeout,
           prompt: brief('prompts/team', { unit, why }) })
  : agent({ profile: unit.worker, stay: false, timeout: job.unitTimeout, schema: CLAIM,
            prompt: brief('prompts/work', { unit, why }) })
).catch(failure => failure)

// Set when a judge halts the job. Every lane reads it before its next unit, so one halt stops
// them all, and the ledger holds it for a later run.
let halted = false
async function halt(unit, ruling) {
  await steward('halt', { unit, ruling })
  halted = true
  return 'halted'
}

// Two attempts per unit, and one more after a question the judge answered. The gate sends an
// attempt back from what it left in the tree. Answers landed, skipped, failed, or halted.
async function unitOf(job, unit) {
  let why
  for (let n = 1, tries = 2; n <= tries; n++) {
    const attempt = await work(job, unit, why)
    if (attempt.outcome === 'blocked') {
      // Answers the question from the job's recorded decisions, or tells the worker the call
      // is its own. One judge for both kinds of worker.
      const ruling = await judge('answer', ['answer', 'skip', 'halt'],
                                 { unit, question: attempt.question })
      if (ruling.outcome === 'halt') return halt(unit, ruling)
      if (ruling.outcome === 'skip') {
        await steward('settle', { unit, ruling })
        return 'skipped'
      }
      why = ruling
      tries = 3
      continue
    }
    const gate = await steward('gate', { unit, attempt }, '1d')
    if (gate.outcome === 'landed') return 'landed'
    why = gate
  }
  await steward('fail', { unit, why })
  return 'failed'
}

// One lane: its units in order. A lane that breaks (a check that died, a job the ledger cannot
// read) opens the steward agent beside the user for this lane, and the other lanes go on.
async function lane(job, l) {
  const units = {}
  try {
    for (const unit of l.units) {
      if (halted) break
      units[unit.id] = await unitOf(job, unit)
      if (units[unit.id] === 'halted') break
      if (units[unit.id] === 'skipped') continue
      const due = await steward('due', { unit })
      if (due.outcome !== 'review') continue
      // Reads a unit's result, or the failures that repeat, against the worker prompt, and
      // writes a sign where the prompt misleads.
      const found = await judge('review', ['keep', 'sign', 'halt'], { unit, due })
      if (found.outcome === 'halt') {
        units[unit.id] = await halt(unit, found)
        break
      }
      if (found.outcome === 'sign') await steward('sign', { unit, sign: found })
    }
    return { lane: l.id, units }
  } catch (failure) {
    await agent({ profile: 'steward',
                  prompt: brief('prompts/break', { lane: l.id, units, failure: told(failure) }) })
      .catch(() => undefined)
    return { lane: l.id, units, broke: failure.reason ?? failure.message }
  }
}

const job = await steward('lanes')
for (const l of job.lanes) {
  for (const unit of l.units) {
    if (!KINDS.includes(unit.kind)) {
      throw new Error('unit ' + unit.id + ' names a kind this workflow does not run')
    }
  }
}

// Every lane works in the job's one worktree, side by side.
const lanes = await pipeline(job.lanes, l => lane(job, l))

// The report runs when the job finished and when a judge halted it. A report that fails ends
// the run failed, and its starter is told.
await agent({ profile: 'opus', stay: false, timeout: '2h',
              prompt: brief('prompts/report', { lanes, halted }) })
return { lanes, halted }

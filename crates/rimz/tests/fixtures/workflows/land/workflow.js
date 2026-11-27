// land: carry a finished team's PR to merge, then rule on what the team left.
//
// DESIGN SKETCH, not runnable: no RimZ loads this format yet. It restates
// ~/.agents/factory/workflows/land/workflow.toml in the form ../../design.md defines.
// Same steps and same scripts. The scripts under checks/ are used as they are today, with two
// changes this sketch assumes: each reads what it is told from input in its context file,
// where today it reads the answer that routed to it, and each is safe to run twice: a send of
// a PR that is already sent, or merged, answers sent.
//
// Different from today's graph:
// - No step is promised to run once. A step the machine died under is started again, and
//   after a failure the whole of land() may run again from its top. Every step of it asks
//   where the world stands (the team, the forge, the notes), so what is done is not redone.
// - The six repairs are this file's own loop count, where today they are a cap on the fix
//   step.
// - After a failure an agent stays open beside the user. It fixes the cause and answers
//   again, and land() goes around once more, where today's debugger resumes the run at a
//   step it names. Three rounds, then the run ends failed.
// - The triage agent ends its own session when the notes hold nothing open: its prompt runs
//   "checks/notes.py after" at the end of each ruling. Today a rest check does that for it.
// - A run that hits its timeout or its launch cap ends failed and opens no agent.
export const meta = {
  name: 'land',
  description: "Carries a finished team's PR to merge, then rules on what the team left",
}

// Every script under checks/ prints one JSON answer, and a nonzero exit throws.
const check = async (script, timeout, input) => JSON.parse(
  (await shell({ command: 'uv run --script checks/' + script, timeout, input })).stdout)
// The same, where the run goes on however the script ended.
const attempt = (script, timeout, input) => check(script, timeout, input).catch(failure => failure)

// A brief: a prompt file, and the values it needs as JSON under it.
const brief = (path, data) =>
  file(path + '.md') + (data ? '\n\n' + JSON.stringify(data, null, 2) : '')
// What an agent is told of a failure: a block's failure carries its own fields (a command's
// log path stands for its output), and any other error its message.
const told = failure => ({
  ...failure, stdout: undefined, reason: failure.reason ?? failure.message })

// One turn of an agent in the checkout. Dead or timed out, the run goes on: the resend after
// it sends whatever the agent pushed. A question stays open in the pane for the user.
const turn = (profile, name, timeout, data) =>
  agent({ profile, stay: false, timeout, prompt: brief('prompts/' + name, data) })
    .catch(failure => failure)

// What keeps the PR out, or its end, in the order the forge decides it: merged or closed, or,
// while it is open, a dequeue, then (while it is not queued) a conflict, a red CI, or a green PR
// that is neither queued nor armed. A conflict reads before any ci reading, since GitHub starts
// no PR CI on a conflicted head. A queued PR waits even when its CI is red.
const CASES = [
  ['merged', 'pr=merged'],
  ['closed', 'pr=closed'],
  ['dequeued', 'pr=open && pr.queue=dequeued'],
  ['conflicted', 'pr=open && pr.queue!=queued && pr.mergeable=conflicting'],
  ['red', 'pr=open && pr.queue!=queued && ci=failed'],
  ['unqueued', 'pr=open && ci=passed && pr.queue=none'],
]
const any = cases => cases.map(([, when]) => '(' + when + ')').join(' || ')

// Fires once any case reads true. A fact not read yet waits.
const FORGE = any(CASES)

// Names what a watch read: the first case that reads true. A case with a fact not read yet is
// passed over, as the watch that fired read past it.
const outcome = ({ readings }) => CASES.find(([, when]) => reads(when, readings))[0]

// Fires once the PR no longer reads unqueued: its case reads false, or a case before it reads
// true, since a PR merged mid-hold can still read green and unqueued.
const UNQUEUED = CASES.findIndex(([name]) => name === 'unqueued')
const BROKEN = '!((' + CASES[UNQUEUED][1] + ') && !(' + any(CASES.slice(0, UNQUEUED)) + '))'

// Watches the forge, and gives a green PR thirty minutes to reach the queue before it counts: a
// watch for the hold to break, whose timeout means it held. A hold that held reads the forge
// again whole before it counts, and one that broke watches from the top.
async function forge() {
  for (;;) {
    const named = outcome(await watch({ when: FORGE, timeout: '3d' }))
    if (named !== 'unqueued') return named
    try {
      await watch({ when: BROKEN, timeout: '30m' })
    } catch (failure) {
      if (failure.cause !== 'timeout') throw failure
      return outcome(await watch({ when: FORGE, timeout: '3d' }))
    }
  }
}

async function land() {
  // Close the team. Whatever it answers, the send sees a team still busy and asks for a sweep.
  await attempt('team.py close', '5m')

  const sent = await attempt('send.py', '20m')
  if (sent.outcome !== 'sent') {
    // The sweeper's leftovers, the AGENTS.md cleanup when the branch touched one, then the
    // send. A check that cannot tell costs a cleanup.
    await turn('sweeper', 'sweep', '3h')
    const touched = await attempt('agentsmd.py', '5m')
    if (touched.outcome !== 'none') await turn('prompter', 'agentsmd', '2h')
    await attempt('send.py resend', '10m')
  }

  // Six repairs: a rebase, a CI fix, or a PR made ready, then the send. Whatever the resend
  // says, the watch comes next: it reads the forge and names what keeps the PR out. Three days
  // in one watch without a merge is a timeout, which ends this round.
  for (let n = 1; ; n++) {
    const pr = { outcome: await forge() }
    if (pr.outcome === 'merged') break
    if (pr.outcome === 'closed') return { pr: 'closed' }
    if (n > 6) throw new Error('the PR is still out after six repairs')
    await turn('fixer', 'fix', '3h', { watch: pr })
    await attempt('send.py resend', '10m')
  }

  await shell({ command: 'cargo clean', timeout: '10m' }).catch(failure => failure)

  // Triage runs only when the team notes hold something to rule on. One small read of the
  // text outside any notes entry settles an unsure sift, and a read that fails costs a triage
  // rather than a lost note. A sift that exits without answering ends this round.
  let notes = await check('notes.py sift', '5m')
  if (notes.outcome === 'unsure') {
    notes = await agent({
      profile: 'haiku', stay: false, timeout: '10m', prompt: brief('checks/notes-read'),
      schema: { type: 'object', required: ['outcome'],
                properties: { outcome: { enum: ['none', 'rule'] } } },
    }).catch(() => ({ outcome: 'rule' }))
  }
  if (notes.outcome === 'none') return { pr: 'merged' }

  // Stays open beside the team notes while something waits on the user, and ends its own
  // session when nothing does. A week closes it. However it closed, one more look records
  // what was settled and what was left in the triage report.
  await agent({ profile: 'triager', timeout: '7d', prompt: brief('prompts/triage') })
    .catch(failure => failure)
  await check('notes.py after', '10m')
  return { pr: 'merged' }
}

// What the repair agent hands in before it ends its session.
const RULING = { type: 'object', required: ['outcome'],
                 properties: { outcome: { enum: ['again', 'stop'] } } }

for (let round = 1; ; round++) {
  try {
    return await land()
  } catch (failure) {
    if (round === 3) throw failure
    // An agent that stays open beside the user and waits for them. It answers again once the
    // cause is fixed. A stop, or a pane closed with no answer, ends the run failed.
    const ruling = await agent({ profile: 'opus', schema: RULING,
                                 prompt: brief('prompts/debug', { failure: told(failure) }) })
      .catch(() => undefined)
    if (ruling?.outcome !== 'again') throw failure
  }
}

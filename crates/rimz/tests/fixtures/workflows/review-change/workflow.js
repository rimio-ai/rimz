// review-change: three reviewers read a finished change side by side, one judge merges what
// they found, and a fixer works until the gate passes. What was found goes back to the agent
// that started the run.
//
// DESIGN SKETCH, not runnable: no RimZ loads this format yet. It follows ../../design.md.
// It restates no workflow of today: it is the short script an agent writes in its own checkout
// and starts itself, by its path or as text handed to the workflow tool:
//   rimz workflow run ./review-change --args base=origin/main
// The run lasts minutes, runs its one command on the host, and hands its result or its failure
// back in the starter's WORKFLOW_REPORT. The five prompt files sit in prompts/ beside this
// file.
//
// Every call here is a tool the starting agent already holds, with the same fields: agent and
// shell. Around them are pipeline, file, and args.
export const meta = {
  name: 'review-change',
  description: 'Three reviews of a finished change, merged, then fixed until the gate passes',
}

// One reviewer per lens.
const LENSES = ['correctness', 'tests', 'docs']

// A brief: a prompt file, and the values it needs as JSON under it.
const brief = (name, data) =>
  file('prompts/' + name + '.md') + '\n\n' + JSON.stringify(data, null, 2)

// A judge: an agent that answers one of the words it is given, with a list when it has one.
// Each takes its prompt file's name as its handle.
const judge = (name, words, data) => agent({
  profile: 'judge', name, stay: false, timeout: '30m', prompt: brief(name, data),
  schema: { type: 'object', required: ['outcome'],
            properties: { outcome: { enum: words }, list: { type: 'array' } } } })

// Each reviewer answers clean, or findings with its list. The three run side by side, and
// their answers come back in the order of the lenses. A reviewer that failed leaves a null:
// the merge reads the ones that answered, and is told how many were asked.
const reviews = await pipeline(LENSES, lens =>
  judge(lens, ['clean', 'findings'], { base: args.base }))
const read = reviews.filter(Boolean)
if (read.length === 0) throw new Error('no reviewer answered')

// Reads the answers and keeps what blocks.
const merged = await judge('merge', ['clean', 'fix'], { reviews: read, asked: LENSES.length })
const result = { found: merged.outcome, blocking: merged.list ?? [] }
if (merged.outcome === 'clean') return result

// Three attempts at a green gate. Each fixer reads the list and the last red gate with its
// log path, and picks up from the tree as the one before left it.
let red
for (let n = 1; n <= 3; n++) {
  await agent({ profile: 'opus', stay: false, timeout: '1h',
                prompt: brief('fix', { blocking: result.blocking, red }) })
  try {
    await shell({ command: 'cargo xtask check', timeout: '30m' })
    return result
  } catch (failure) {
    // A red gate is an answer. Anything else, a timeout say, ends the run.
    if (failure.cause !== 'exit') throw failure
    red = { code: failure.code, log: failure.output }
  }
}
throw new Error('the gate is still red after three fixes')

# Team consensus

A team turns one user request into shipped work by moving it through the pipeline's stages. Each stage has one owner and is an obligation to leave specific information behind: the pipeline below says what each stage must produce, and your craft above says how. Your launch reminder names the pipeline and the stages each seat owns.

## The turn

You act only on input. A prompt re-invokes you; between prompts you do nothing. Skill(rimz-message) has the header blocks a prompt arrives in and how to send one.

- A `STAGE` block opens a stage you own. The board says where the work stands and the pipeline names the files you open first.
- Anything else is a message from a teammate or the user (see Speaking).

Every turn has the same shape.

1. Read `blackboard.md`. It says which stage is live and what the run has decided; the stage files say the rest.
2. Do the stage work your craft defines. Its product goes in your stage file.
3. End the turn with a flip, a message, or a rest. A finished stage is a flip. A question, an answer, or a changed file a teammate builds on is a message. A turn that ends with neither is a rest: nothing is pending, and the next prompt re-invokes you.

The flip is `rimz teams flip <Stage> "<note>"`, naming the stage that opens next by its exact name. The note says what is done; the reasoning stays in your stage file, since a judging stage reads the board before it reads the author. The flip wakes that stage's owner at its next turn boundary. A flip to a stage you own wakes nobody: carry on into it in the same turn.

## Memory

The team remembers through files in the worktree root: `blackboard.md` and one `<stage>-notes.md` per stage, the same layout under every roster.

### The blackboard

`blackboard.md` holds the run's state and history, and it is where the user looks to see where the run stands.

```
# Blackboard
Stage: <stage> (@<owner>)

## Goal
<the user's request, restated once by the leader>

## Decisions
<append-only: who decided what, and why>

## Progress
<append only: stage flip log>

## Result
<verification evidence, the link to what shipped>
```

The board is the leader's first product. On its first turn the leader writes `blackboard.md` with the Goal filled in — the request restated, and the path of any document it arrived as — above the empty Decisions, Progress, and Result sections. Then it flips to the first stage with the note `board opened; <aim>`. The flip adds the Stage line and the first Progress record, and wakes that stage's owner onto a board that already says what the run is for; the leader carries on into the stage work when it owns that stage.

The Stage line and the Progress records belong to `rimz teams flip`. Progress holds stage changes and nothing else; what happened inside a stage lives in the stage file. Every other section appends: when the picture changes, add a line with the newer truth and leave the old ones standing.

### Stage files

`<stage>-notes.md`, one per stage, lowercase (Explore writes `explore-notes.md`), is the stage's product and its owner's working memory. Only the owner writes it, across every turn and subagent of that stage. Any teammate may read it unless the pipeline closes it to them.

The pipeline says what the file must hold. Around that it carries the narrative: findings, plans, reports, reasoning, and where the stage stands between turns, so a teammate or a restarted you can pick the stage up from the file alone. A stage whose product already lives in git, in the shipped artifact, or in the board's Result may leave no file.

### Reflection

When you complete a stage, run Skill(reflect) on it and end the stage file with a terse `## Reflection`: each entry one fix and where it lands, and none for a smooth stage. A defect in your file that a later stage surfaces is your entry, written when you amend the file. An entry whose fix lives in the worktree goes to the Implement owner while the run is still open, so it lands as a commit.

Reflect is the last stage of every pipeline. Its owner runs Skill(reflect) in distill mode, writes `reflect-notes.md` in the shape that skill gives, and flips to `Done`. `Done` closes the board and wakes nobody; a later flip reopens it.

## Speaking

A message carries a question, an answer, or a pointer to a changed file someone builds on; the substance stays in the file. `plan amended at plan-notes.md:40-58, re-read before step 3` is a whole message. Send one when what you have changes something for the reader: as many rounds as the work needs, none for courtesy.

The leader sends every message with `--steer`. What it sends is user intent or a changed plan, and a parked message would land only after the receiver finished a full turn against the old truth.

A request the user sends you directly is yours: do it in this turn, record what it changed in your stage file, and message the teammate whose work it changes.

The user is reached only through the leader. Where your craft says ask the user, message the teammate who owns the answer and build on their reply. A call only the user can make (intent, scope, a tradeoff only they can price) goes to the leader, and the work that depends on it holds until the user rules. Raise it early, while the answer can still shape the work.

The board and the PR are the report: the user reads the PR, the commits, the running thing, and the board.

## Disagreement

These are defaults; a pipeline may reassign any of them.

- Within a stage, the owner decides. Design and intent belong to the design stage's owner where the pipeline has one. Push back with evidence; their call is final.
- The code and the live state decide correctness. Bring `file:line` or observed output, and concede as soon as it shows you wrong.
- A judging stage's measurements stand for the run. The leader and every later owner read the judge's numbers instead of reproducing them, and a doubt is a question to the judge.
- Two rounds bound a dispute. Past them, between a judge and the stage it judges, the judge rules; a design or intent clash goes to the design stage's owner; between any other two roles, the leader rules.

## Recovery

A crash, restart, or compaction can interrupt the team at any point. The state survives in the board, the stage files, and git, so re-derive it from those: the Stage line says which stage is live, Progress replays the hand-offs, each stage file says where its stage stopped, and git says what shipped. On resume the owner of the live stage gets a `STAGE` re-wake: reread the board and continue from where your stage file stops. A finished stage whose flip never landed is flipped now; a wrong flip is corrected by flipping again. Still unsure, ask the owner of the file.

## Precedence

The pipeline binds this consensus, and this consensus binds your craft. An explicit pipeline rule (a reader barred from a file, an escalation reassigned) wins over these defaults, and these defaults win over the craft, which was written for a user in the loop. Inside the stage you own, the craft's solo constraints hold, widened by the shared files: a read-only craft writes the board and its stage file.

The pipeline orders information, and a turn may run ahead of it when the work asks: pull the judge in early on a scary change, reopen the plan when an assumption breaks mid-build. Three things never bend: the user's single channel through the leader, the judge's independence, and a flip at every hand-off.

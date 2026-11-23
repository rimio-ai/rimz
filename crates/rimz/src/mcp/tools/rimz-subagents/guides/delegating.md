# Delegating to a subagent

## What to delegate

A child takes work whose churn you do not want in your window: a task a profile's description matches, independent pieces that run in parallel, or an answer that takes reading across many files when you need only the conclusion. Work you can settle yourself stays with you: a file, symbol, or value you already know, a question that spans two or three files. Once work is delegated, wait for its result instead of running the same work yourself.

Pick the profile whose description matches the task; a general profile takes the bounded task no sharper profile fits. Launch independent children in one turn, one `subagent` call each, so the batch costs you one turn.

## The brief

The `prompt` is the only context the child has. Nothing comes back before the child's final report, and a follow-up costs the child another turn. So the brief carries:

- The goal, and what you have already ruled out.
- The files, entry points, and docs worth reading, named by path instead of retyped.
- The scope, explicit and narrow: whether the child is read-only or may edit, and which files. Children share your checkout, so each child that edits gets files no other child touches.
- Exactly what to return in that one message: a table, a list of `file:line`, a yes or no with evidence.

When you cannot state these clearly, you do not understand the task well enough to hand it off yet.

A child handed your conclusion tends to return it confirmed. When you want a read independent of yours (a review, a second opinion), the brief gives the material and the question, and leaves out your conclusion and what you ruled out on the way to it.

## Getting the result

Take the result through the `SUBAGENT_REPORT` message, which blocks no turn of yours: once every child you have running has settled, it lands with each child's status and the file holding its final message. The size beside each file is what reading it costs your window: read a small one whole, and a large one in the parts you need. Before an edit or a statement to the user rests on a child's claim, open the anchor the claim cites, and confirm a claim that something is absent with a search of your own.

`join` is for the one result the next step cannot start without: it blocks your turn and answers the child's whole final message. A child that outruns the join keeps running and comes back through the report. A child joined in your turn drops out of the report.

## Lifecycle

`tasks` lists your children. Read it before you relaunch a child whose launch looks lost: it may be running or done. A child is cut off at the `timeout` its launch set, else at its profile's timeout, 30 minutes by default, and each follow-up turn gets that limit again; a brief that needs longer takes a `timeout` sized to it, and independent questions over different areas go to one child per area. `stop` ends one child.

## Follow-ups

A child keeps its session, so a follow-up question goes to the same child as a message to `@<petname>`, instead of a fresh launch that would redo the reading; its answer arrives as another `SUBAGENT_REPORT`. A message reaches a running child once its turn ends, and reaches an ended child by resuming its session. A different task is a fresh child.

# Delegating to a subagent

## Choosing what and who

Once work is delegated, wait for its result instead of running the same work yourself.

Pick the profile whose description matches the task, or a general profile when no sharper one fits. Launch independent children in one turn, one `subagent` call each, so the batch costs you one turn.

## Writing the brief

Nothing comes back before the child's final report, and a follow-up costs another turn. Make the brief carry:

- The goal, and what you have already ruled out.
- The files, entry points, and docs worth reading, named by path instead of retyped.
- The explicit scope: whether the child is read-only or may edit, and which files.
- Exactly what to return in that one message: a table, a list of `file:line`, a yes or no with evidence.

When you cannot state these clearly, you do not understand the task well enough to hand it off yet.

A child handed your conclusion tends to return it confirmed. For an independent read (a review, a second opinion), give the material and the question and leave out your conclusion and what you ruled out on the way to it.

## Reading the report and joining

The size beside each file in `SUBAGENT_REPORT` is what reading it costs your window: read a small one whole, and a large one in the parts you need. Before an edit or a statement to the user rests on a child's claim, open the anchor the claim cites, and confirm a claim that something is absent with a search of your own.

Use `join` only for the one result the next step cannot start without: it blocks your turn and returns the child's whole result. A child that outruns `join` keeps running and comes back through the report; a child joined in your turn drops out of the report.

## Follow-ups and retries

Check `tasks` before relaunching a child whose launch looks lost: it may be running or done. Each follow-up turn gets the child's timeout again; size `timeout` to a brief that needs longer, and split independent questions across separate children.

A follow-up message reaches a running child once its turn ends, resumes an ended child's session, and reports back as another `SUBAGENT_REPORT`.

Launch a persistent agent: a peer in its own context and background tab that the user can watch and talk to. It keeps its context, reports when its turn settles, and takes follow-ups until stopped. Use it for work that wants its own worktree, a pull request checkout, a specialist profile's craft, or a mind that outlives one brief. For a multi-seat team, use `team`.

For one task that closes once it reports, pass `stay: false`; for an agent that takes follow-ups and closes once it has rested a while after its first turn, pass a duration such as `stay: "10m"`. Give a `schema` when you need a structured answer, which the agent hands in with `rimz submit` and the report and `join` return.

Give a repo-changing agent its own `worktree` (whatever it must see must already be on that base branch); run a review, plan, or question in this checkout.

When `report` says an `AGENT_REPORT` comes, one report lands once none of your agents is mid-turn; otherwise `join` is the way to its answers. Send follow-ups to `@<handle>`, or `@<handle>#<worktree>` in a worktree. Before a first launch, call `guide` with `rimz-agents/launching`.

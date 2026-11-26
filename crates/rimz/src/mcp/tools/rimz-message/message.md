Your tool for reaching another agent in the room: a hand-off, a question, an answer, or a correction. `to` is its `@handle`, or `@handle#channel` for one in another channel. A handle that matches several agents is refused with their addresses unless `all` sends to each, and an address that matches none is refused with the live agents. A message to a subagent you launched that has ended resumes its session.

`mode` `park`, the default, fits almost everything: the text lands now when the receiver is at rest, and otherwise at its next turn boundary. `steer` writes into its running turn now; reserve it for when waiting costs a turn of wrong work, because the ground moved under what the receiver is doing or your answer unblocks the turn it is running. `interrupt` stops the running turn, then types the text as its next prompt; on Codex, Pi, and Antigravity, text the receiver had queued comes back into its composer and goes out joined to yours.

The answer lists each message with its id and `status`: `sent` (typed into the receiver's pane now), `queued` (held, with why in `detail` when RimZ knows), or `not_sent` (with why in `detail`). No status says the receiver has read it. `reply_wanted` marks the receiver's header as wanting a message back, which arrives as a new prompt, so end your turn rather than wait for it.

A message to you arrives as a prompt that opens with a header:

    Type: AGENT_MESSAGE
    From: @<handle>[#channel] (<label>)[, wants a reply]
    Content:
    <message>

`From` names the sender, never the receiver: the block is for you, and `(<label>)` is the sender's profile or agent kind. `AGENT_MESSAGE` is another agent. `SUBAGENT_REPORT`, `AGENT_REPORT`, and `TEAM_REPORT` come from `@rimz` when what you launched settles, naming the files to read, and `STAGE` opens a stage of a team you sit on; none of these needs a reply. A prompt may carry several header blocks, each its own message; a prompt with none is the user typing in your pane or RimZ's own text (such as `continue`), answered in your turn text either way. Answer an agent with `message` to its `From` address without the label: your turn text reaches only the agent that launched you.

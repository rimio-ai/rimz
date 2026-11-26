Your background-shell tool: start a command outside your turn, then go on with other work or end your turn; when it exits, its outcome reaches you as a message, mid-turn or as your next prompt, with your context intact. No tool timeout cuts it off, and your cache stays warm while it runs. Reach for it with a command that could outrun your tool timeout (a build, a test suite, a deploy) or one you would background and poll; your provider's own backgrounding works too, and a command of a few seconds stays in your shell.

The wake names the outcome and the output file: `exit 1 after 12m · output: <path> (~14k tokens, 1210 lines)`. `join` with its id blocks until the command exits and answers that outcome in place of the message.

Write any other wait as the command:
- a state: `until curl -sf localhost:8080/health; do sleep 2; done`
- a line in a log: `tail -n0 -F app.log | grep -m1 -F 'ready'`
- a process: `while kill -0 4242; do sleep 1; done`
- a delay: `sleep 1200`, which reads as work on your card and does not survive a reboot

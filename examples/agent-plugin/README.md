# ScriptBot agent plugin

This bundle is a complete process-plugin example. `fake-agent.js` emits canonical lifecycle and context envelopes, `probes/spend.js` turns the fixture transcript into priced spend entries, and `probes/account` reports a local demo account.

Copy the bundle into the machine plugin directory and validate it:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/rimz/agents.d"
cp -R examples/agent-plugin "${XDG_CONFIG_HOME:-$HOME/.config}/rimz/agents.d/scriptbot"
chmod +x "${XDG_CONFIG_HOME:-$HOME/.config}/rimz/agents.d/scriptbot/fake-agent.js"
chmod +x "${XDG_CONFIG_HOME:-$HOME/.config}/rimz/agents.d/scriptbot/probes/account"
rimz agents register --check
```

Launch it with `rimz agents scriptbot "run the demo"`. A real integration installs its native hook or extension itself; that shim translates native payloads to the envelope documented in [the agent plugin reference](../../docs/reference/agent-plugins.md) and invokes `rimz hooks feed --source scriptbot` in the same way as this example.

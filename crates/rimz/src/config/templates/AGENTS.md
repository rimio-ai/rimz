# Config templates

Local contract for `crates/rimz/src/config/templates/`. Extends [config/AGENTS.md](../AGENTS.md).

- A change to a file here runs `cargo xtask test 'config::template_tests'`: the test uncomments each template's default lines and parses the result, so a prose comment line that begins with `[` reads as a table header.

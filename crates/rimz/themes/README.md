# Bundled Alacritty Themes

This directory vendors 533 Alacritty TOML themes from [mbadolato/iTerm2-Color-Schemes](https://github.com/mbadolato/iTerm2-Color-Schemes) for Rimz sidebar theme selection.

Source revision: local checkout at 8c84dd1a859f36ec8601b082bd97b4ec888f0f4d.

Refresh with `cargo xtask theme-refresh`. Set `RIMZ_THEMES_DIR=/path/to/iTerm2-Color-Schemes` to refresh from a local checkout without network access.

The theme files are data embedded into the Rimz binary at build time; they are not linked Rust dependencies.

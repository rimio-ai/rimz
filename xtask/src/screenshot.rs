use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process;
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};

use crate::files::remove_stale_file;
use crate::is_help_flag;
use crate::runner::ensure_success;

const SCREENSHOT_CONFIG: &str = "xtask/assets/ghostty-tokyonight.json";
const SCREENSHOT_DIR: &str = "target/screenshots";
const FREEZE_VERSION: &str = "0.2.2";
const NERD_FONTS_VERSION: &str = "3.4.0";
/// Output width for rendered PNGs. The sidebar reads at roughly 30% of a 1920px
/// screen, so freeze's intrinsic raster (cell-sized, ~1x) is scaled to this target.
/// rsvg rasterizes the SVG vectors straight at this width, so glyphs stay crisp
/// without a supersample pass; `--keep-aspect-ratio` scales uniformly so cells never
/// distort.
const SCREENSHOT_TARGET_WIDTH_PX: &str = "576";

#[derive(Debug, Default)]
struct CaptureScreenshotOptions {
    lines: Option<u16>,
    output: Option<PathBuf>,
}

#[derive(Debug)]
struct StateScreenshotOptions {
    state: SidebarScreenshotState,
    width: u16,
    height: u16,
    theme_mode: Option<String>,
    theme_scheme: Option<String>,
    output: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SidebarScreenshotState {
    Empty,
    Fleet,
    Provider,
    Cockpit,
    Focus,
    Economy,
    Reach,
}

impl SidebarScreenshotState {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "empty" => Ok(Self::Empty),
            "fleet" => Ok(Self::Fleet),
            "provider" => Ok(Self::Provider),
            "cockpit" => Ok(Self::Cockpit),
            "focus" => Ok(Self::Focus),
            "economy" => Ok(Self::Economy),
            "reach" => Ok(Self::Reach),
            other => {
                bail!(
                    "unknown screenshot state `{other}`; expected empty, fleet, provider, cockpit, focus, economy, or reach"
                )
            }
        }
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Fleet => "fleet",
            Self::Provider => "provider",
            Self::Cockpit => "cockpit",
            Self::Focus => "focus",
            Self::Economy => "economy",
            Self::Reach => "reach",
        }
    }
}

pub(crate) fn screenshot(root: &Path, args: &[String]) -> Result<()> {
    let Some(subcmd) = args.first().map(String::as_str) else {
        print_screenshot_help();
        return Ok(());
    };
    if is_help_flag(subcmd) {
        print_screenshot_help();
        return Ok(());
    }
    if args.iter().skip(1).any(|arg| is_help_flag(arg)) {
        print_screenshot_subcommand_help(subcmd)?;
        return Ok(());
    }

    match subcmd {
        "list" => {
            ensure_no_extra_args("screenshot list", &args[1..])?;
            rimz_status(root, &os_args(["pane", "list", "--json"]))
        }
        "live" => {
            let opts = parse_capture_screenshot_options(&args[1..])?;
            ensure_screenshot_prerequisites()?;
            let ansi = capture_pane_ansi(root, "sidebar", opts.lines)?;
            let output = screenshot_output_path(root, opts.output, "live")?;
            write_screenshot_png(root, &ansi, &output)?;
            print_screenshot_path(&output);
            Ok(())
        }
        "pane" => {
            let Some(pane_id) = args.get(1) else {
                bail!("screenshot pane requires a pane id");
            };
            let opts = parse_capture_screenshot_options(&args[2..])?;
            ensure_screenshot_prerequisites()?;
            let ansi = capture_pane_ansi(root, pane_id, opts.lines)?;
            let output = screenshot_output_path(
                root,
                opts.output,
                &format!("pane-{}", sanitize_file_stem(pane_id)),
            )?;
            write_screenshot_png(root, &ansi, &output)?;
            print_screenshot_path(&output);
            Ok(())
        }
        "state" => {
            let opts = parse_state_screenshot_options(&args[1..])?;
            ensure_screenshot_prerequisites()?;
            let ansi = render_state_ansi(
                root,
                opts.state,
                opts.width,
                opts.height,
                opts.theme_mode.as_deref(),
                opts.theme_scheme.as_deref(),
            )?;
            let output = screenshot_output_path(root, opts.output, opts.state.as_str())?;
            write_screenshot_png(root, &ansi, &output)?;
            print_screenshot_path(&output);
            Ok(())
        }
        other => bail!("unknown screenshot subcommand `{other}`"),
    }
}

#[expect(
    clippy::print_stdout,
    reason = "xtask screenshot help text is a command stdout contract"
)]
fn print_screenshot_help() {
    println!("cargo xtask screenshot");
    println!();
    println!("Render sidebar ANSI captures to PNG with freeze.");
    println!();
    println!("Usage:");
    println!("  cargo xtask screenshot list");
    println!("  cargo xtask screenshot live [--lines N] [--output PATH]");
    println!("  cargo xtask screenshot pane <id> [--lines N] [--output PATH]");
    println!(
        "  cargo xtask screenshot state <empty|fleet|provider|cockpit|focus|economy|reach> [--width W] [--height H] [--theme-mode auto|truecolor|256] [--theme-scheme NAME] [--output PATH]"
    );
}

fn print_screenshot_subcommand_help(subcmd: &str) -> Result<()> {
    match subcmd {
        "list" => print_screenshot_list_help(),
        "live" => print_screenshot_live_help(),
        "pane" => print_screenshot_pane_help(),
        "state" => print_screenshot_state_help(),
        other => bail!("unknown screenshot subcommand `{other}`"),
    }
    Ok(())
}

#[expect(
    clippy::print_stdout,
    reason = "xtask screenshot help text is a command stdout contract"
)]
fn print_screenshot_list_help() {
    println!("cargo xtask screenshot list");
    println!();
    println!("Print the current `rimz pane list --json` output.");
}

#[expect(
    clippy::print_stdout,
    reason = "xtask screenshot help text is a command stdout contract"
)]
fn print_screenshot_live_help() {
    println!("cargo xtask screenshot live [--lines N] [--output PATH]");
    println!();
    println!("Capture the live rimz-sidebar pane without focusing it and render a PNG.");
}

#[expect(
    clippy::print_stdout,
    reason = "xtask screenshot help text is a command stdout contract"
)]
fn print_screenshot_pane_help() {
    println!("cargo xtask screenshot pane <id> [--lines N] [--output PATH]");
    println!();
    println!("Capture any pane by normalized pane id and render a PNG.");
}

#[expect(
    clippy::print_stdout,
    reason = "xtask screenshot help text is a command stdout contract"
)]
fn print_screenshot_state_help() {
    println!(
        "cargo xtask screenshot state <empty|fleet|provider|cockpit|focus|economy|reach> [--width W] [--height H] [--theme-mode auto|truecolor|256] [--theme-scheme NAME] [--output PATH]"
    );
    println!();
    println!("Render a deterministic sidebar fixture frame and write a PNG.");
}

fn parse_capture_screenshot_options(args: &[String]) -> Result<CaptureScreenshotOptions> {
    let mut opts = CaptureScreenshotOptions::default();
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--lines" => {
                let value = required_option_value(args, index, "--lines")?;
                opts.lines = Some(parse_u16_flag(value, "--lines")?);
                index += 2;
            }
            "-o" | "--output" => {
                let value = required_option_value(args, index, "--output")?;
                opts.output = Some(PathBuf::from(value));
                index += 2;
            }
            other => bail!("unknown screenshot option `{other}`"),
        }
    }
    Ok(opts)
}

fn parse_state_screenshot_options(args: &[String]) -> Result<StateScreenshotOptions> {
    let Some(state) = args.first() else {
        bail!(
            "screenshot state requires empty, fleet, provider, cockpit, focus, economy, or reach"
        );
    };
    let mut opts = StateScreenshotOptions {
        state: SidebarScreenshotState::parse(state)?,
        width: 54,
        height: 34,
        theme_mode: None,
        theme_scheme: None,
        output: None,
    };
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--width" => {
                let value = required_option_value(args, index, "--width")?;
                opts.width = parse_u16_flag(value, "--width")?;
                index += 2;
            }
            "--height" => {
                let value = required_option_value(args, index, "--height")?;
                opts.height = parse_u16_flag(value, "--height")?;
                index += 2;
            }
            "--theme-mode" => {
                let value = required_option_value(args, index, "--theme-mode")?;
                opts.theme_mode = Some(value.to_owned());
                index += 2;
            }
            "--theme-scheme" => {
                let value = required_option_value(args, index, "--theme-scheme")?;
                opts.theme_scheme = Some(value.to_owned());
                index += 2;
            }
            "-o" | "--output" => {
                let value = required_option_value(args, index, "--output")?;
                opts.output = Some(PathBuf::from(value));
                index += 2;
            }
            other => bail!("unknown screenshot option `{other}`"),
        }
    }
    Ok(opts)
}

fn required_option_value<'a>(args: &'a [String], index: usize, flag: &str) -> Result<&'a str> {
    args.get(index + 1)
        .map(String::as_str)
        .filter(|value| !value.starts_with('-'))
        .with_context(|| format!("{flag} requires a value"))
}

fn parse_u16_flag(value: &str, flag: &str) -> Result<u16> {
    value
        .parse::<u16>()
        .with_context(|| format!("parsing {flag} value `{value}`"))
}

fn ensure_no_extra_args(command: &str, args: &[String]) -> Result<()> {
    if args.is_empty() {
        return Ok(());
    }
    bail!("{command} takes no arguments")
}

fn capture_pane_ansi(root: &Path, pane_id: &str, lines: Option<u16>) -> Result<Vec<u8>> {
    let mut args = os_args(["pane", "capture", pane_id, "--ansi"]);
    if let Some(lines) = lines {
        args.push(OsString::from("--lines"));
        args.push(OsString::from(lines.to_string()));
    }
    rimz_output(root, &args)
}

fn render_state_ansi(
    root: &Path,
    state: SidebarScreenshotState,
    width: u16,
    height: u16,
    theme_mode: Option<&str>,
    theme_scheme: Option<&str>,
) -> Result<Vec<u8>> {
    let mut args = vec![
        OsString::from("sidebar"),
        OsString::from("fixture"),
        OsString::from(state.as_str()),
        OsString::from("--width"),
        OsString::from(width.to_string()),
        OsString::from("--height"),
        OsString::from(height.to_string()),
    ];
    if let Some(mode) = theme_mode {
        args.push(OsString::from("--theme-mode"));
        args.push(OsString::from(mode));
    }
    if let Some(scheme) = theme_scheme {
        args.push(OsString::from("--theme-scheme"));
        args.push(OsString::from(scheme));
    }
    rimz_output_with_env(root, &args, &[("COLORTERM", "truecolor")], &["NO_COLOR"])
}

fn ensure_screenshot_prerequisites() -> Result<()> {
    let freeze_status = Command::new("freeze")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match freeze_status {
        Ok(status) if status.success() => {}
        _ => bail!(
            "{}",
            screenshot_bootstrap_message("freeze is not installed")
        ),
    }

    let rsvg_status = Command::new("rsvg-convert")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match rsvg_status {
        Ok(status) if status.success() => {}
        _ => bail!(
            "{}",
            screenshot_bootstrap_message("rsvg-convert is not installed")
        ),
    }

    if !jetbrains_nerd_font_available()? {
        bail!(
            "{}",
            screenshot_bootstrap_message("JetBrainsMono Nerd Font Mono is not installed")
        );
    }
    Ok(())
}

fn jetbrains_nerd_font_available() -> Result<bool> {
    let output = match Command::new("fc-match")
        .args(["-f", "%{family}\n", "JetBrainsMono Nerd Font Mono"])
        .output()
    {
        Ok(output) => output,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err).context("running fc-match"),
    };
    if !output.status.success() {
        return Ok(false);
    }
    let family = String::from_utf8_lossy(&output.stdout);
    let family = family.to_lowercase();
    Ok(family.contains("jetbrains") && family.contains("nerd"))
}

fn screenshot_bootstrap_message(reason: &str) -> String {
    format!(
        "{reason}\n\nInstall screenshot prerequisites:\n  mkdir -p ~/.local/bin ~/.local/share/fonts\n  tmp=\"$(mktemp -d)\"\n  curl -fsSL https://github.com/charmbracelet/freeze/releases/download/v{FREEZE_VERSION}/freeze_{FREEZE_VERSION}_Linux_x86_64.tar.gz | tar -xz -C \"$tmp\"\n  install -m 0755 \"$tmp/freeze_{FREEZE_VERSION}_Linux_x86_64/freeze\" ~/.local/bin/freeze\n  curl -fsSL https://github.com/ryanoasis/nerd-fonts/releases/download/v{NERD_FONTS_VERSION}/JetBrainsMono.tar.xz | tar -xJ -C ~/.local/share/fonts\n  fc-cache -f\n  sudo apt-get install -y librsvg2-bin\n  freeze --version\n  rsvg-convert --version\n  fc-match \"JetBrainsMono Nerd Font Mono\""
    )
}

fn screenshot_output_path(root: &Path, output: Option<PathBuf>, label: &str) -> Result<PathBuf> {
    let path = match output {
        Some(path) if path.is_absolute() => path,
        Some(path) => root.join(path),
        None => {
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .context("system clock before Unix epoch")?
                .as_secs();
            root.join(SCREENSHOT_DIR).join(format!(
                "rimz-sidebar-{}-{stamp}-{}.png",
                sanitize_file_stem(label),
                process::id()
            ))
        }
    };
    if path.extension().and_then(OsStr::to_str) != Some("png") {
        bail!(
            "screenshot output path must end in .png: {}",
            path.display()
        );
    }
    Ok(path)
}

/// Glyphs Ghostty paints from its built-in sprite renderer — box-drawing and the
/// Symbols for Legacy Computing block — that JetBrainsMono Nerd Font Mono carries no
/// outline for. freeze rasterizes through librsvg, which can only draw glyphs the
/// font file actually contains, so an unmapped sprite glyph falls back to a
/// mismatched font at a different advance width and breaks column alignment. Each
/// entry maps such a glyph to the nearest glyph the font has. Ordinary symbols the
/// font also lacks (arrows, braille, geometric shapes) fall back cleanly on their
/// own and stay untouched.
const SPRITE_GLYPH_FALLBACKS: &[(char, &str)] = &[
    ('\u{1FB87}', "\u{2595}"), // RIGHT ONE QUARTER BLOCK -> RIGHT ONE EIGHTH BLOCK
];

/// Substitute the sprite glyphs the screenshot font cannot draw (see
/// [`SPRITE_GLYPH_FALLBACKS`]). The sidebar emits them because a live Ghostty
/// terminal renders them itself; this keeps the captured frame aligned where it is
/// rasterized through a plain font instead.
fn remap_sprite_glyphs(ansi: &[u8]) -> Vec<u8> {
    let Ok(text) = std::str::from_utf8(ansi) else {
        return ansi.to_vec();
    };
    let mut text = text.to_owned();
    for (from, to) in SPRITE_GLYPH_FALLBACKS {
        text = text.replace(*from, to);
    }
    text.into_bytes()
}

fn write_screenshot_png(root: &Path, ansi: &[u8], output: &Path) -> Result<()> {
    let ansi = remap_sprite_glyphs(ansi);
    let parent = output
        .parent()
        .with_context(|| format!("{} has no parent directory", output.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    let file_name = output
        .file_name()
        .with_context(|| format!("{} has no file name", output.display()))?
        .to_string_lossy();
    let staged_png = parent.join(format!(".{file_name}.tmp.{}.png", process::id()));
    let staged_svg = parent.join(format!(".{file_name}.tmp.{}.svg", process::id()));
    remove_stale_file(&staged_png)?;
    remove_stale_file(&staged_svg)?;

    let config = root.join(SCREENSHOT_CONFIG);
    let args = vec![
        OsString::from("--config"),
        config.as_os_str().to_owned(),
        OsString::from("--output"),
        staged_svg.as_os_str().to_owned(),
    ];
    let mut child = Command::new("freeze")
        .args(&args)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .context("running `freeze`")?;
    {
        let stdin = child.stdin.as_mut().context("freeze stdin was not piped")?;
        stdin
            .write_all(&ansi)
            .context("writing ANSI frame to freeze")?;
    }
    drop(child.stdin.take());
    let status = child.wait().context("waiting for freeze")?;
    ensure_success("freeze", &args, status)?;
    if !staged_svg.is_file() {
        bail!("freeze did not write {}", staged_svg.display());
    }

    let rsvg_args = vec![
        OsString::from("--width"),
        OsString::from(SCREENSHOT_TARGET_WIDTH_PX),
        OsString::from("--keep-aspect-ratio"),
        OsString::from("-o"),
        staged_png.as_os_str().to_owned(),
        staged_svg.as_os_str().to_owned(),
    ];
    let status = Command::new("rsvg-convert")
        .args(&rsvg_args)
        .current_dir(root)
        .status()
        .context("running `rsvg-convert`")?;
    ensure_success("rsvg-convert", &rsvg_args, status)?;
    if !staged_png.is_file() {
        bail!("rsvg-convert did not write {}", staged_png.display());
    }
    fs::rename(&staged_png, output).with_context(|| format!("installing {}", output.display()))?;
    remove_stale_file(&staged_svg)
}

fn rimz_status(root: &Path, args: &[OsString]) -> Result<()> {
    let status = rimz_command(root, args)
        .status()
        .context("running `rimz`")?;
    ensure_success("rimz", args, status)
}

fn rimz_output(root: &Path, args: &[OsString]) -> Result<Vec<u8>> {
    rimz_output_with_env(root, args, &[], &[])
}

fn rimz_output_with_env(
    root: &Path,
    args: &[OsString],
    envs: &[(&str, &str)],
    removed_envs: &[&str],
) -> Result<Vec<u8>> {
    let mut command = rimz_command(root, args);
    command.envs(envs.iter().copied());
    for key in removed_envs {
        command.env_remove(key);
    }
    let output = command.output().context("running `rimz`")?;
    if output.status.success() {
        return Ok(output.stdout);
    }
    let rendered_args = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let stderr = String::from_utf8_lossy(&output.stderr);
    bail!("command failed: rimz {rendered_args}\n{stderr}");
}

fn rimz_command(root: &Path, args: &[OsString]) -> Command {
    let mut command = if let Some(bin) = env::var_os("RIMZ_BIN") {
        Command::new(bin)
    } else {
        let mut command = Command::new("cargo");
        command.args(["run", "--quiet", "-p", "rimz", "--bin", "rimz", "--"]);
        command
    };
    command.args(args).current_dir(root);
    command
}

fn os_args<const N: usize>(args: [&str; N]) -> Vec<OsString> {
    args.into_iter().map(OsString::from).collect()
}

fn sanitize_file_stem(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch);
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').to_owned()
}

#[expect(
    clippy::print_stdout,
    reason = "screenshot command prints the produced image path"
)]
fn print_screenshot_path(path: &Path) {
    println!("{}", path.display());
}

#[cfg(test)]
mod tests;

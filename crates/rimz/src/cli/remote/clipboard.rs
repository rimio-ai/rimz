//! The laptop's clipboard, read once per Ctrl+V for files or an image to
//! upload.
//!
//! A reader is a subprocess whose stdout is `files` and one absolute path per
//! line, `image` and the path of a PNG it wrote, or `none`. On macOS it is
//! `osascript` running [`MAC_CLIPBOARD_SCRIPT`]; `RIMZ_REMOTE_CLIPBOARD_BIN`
//! names a replacement, and elsewhere it is the only reader. The read is
//! bounded, so a hung reader is killed and its key forwards.

use std::path::PathBuf;
use std::time::Duration;

const CLIPBOARD_BIN_ENV: &str = "RIMZ_REMOTE_CLIPBOARD_BIN";
/// A reader that has not answered by now is killed.
const CLIPBOARD_WAIT: Duration = Duration::from_secs(2);

/// File URLs come first: Finder publishes a copied file's icon as an image
/// beside its URL.
const MAC_CLIPBOARD_SCRIPT: &str = r"ObjC.import('AppKit');
function run(argv) {
  var pb = $.NSPasteboard.generalPasteboard, items = pb.pasteboardItems, paths = [];
  for (var i = 0; i < items.count; i++) {
    var s = items.objectAtIndex(i).stringForType('public.file-url');
    if (!s.isNil()) paths.push(ObjC.unwrap($.NSURL.URLWithString(s).path));
  }
  if (paths.length) return 'files\n' + paths.join('\n');
  var png = pb.dataForType('public.png');
  if (png.isNil()) {
    var tiff = pb.dataForType('public.tiff');
    if (tiff.isNil()) return 'none';
    png = $.NSBitmapImageRep.imageRepWithData(tiff).representationUsingTypeProperties(4, $.NSDictionary.dictionary);
  }
  png.writeToFileAtomically(argv[0], true);
  return 'image\n' + argv[0];
}";

/// What the clipboard holds for an upload.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Clipboard {
    Files(Vec<PathBuf>),
    /// A PNG the reader wrote, which the caller removes.
    Image(PathBuf),
    /// Nothing to upload, no reader, or a reader that failed.
    Nothing,
}

/// Reads the clipboard, writing an image into a fresh file under `scratch`.
pub(super) fn read(scratch: &std::path::Path) -> Clipboard {
    let png = scratch.join("clipboard.png");
    let Some(spec) = reader(&png) else {
        return Clipboard::Nothing;
    };
    let read = match spec.run_with_timeout(CLIPBOARD_WAIT) {
        Ok(output) => parse(&String::from_utf8_lossy(&output.stdout)),
        Err(err) => {
            tracing::debug!(error = %err, "clipboard reader failed");
            Clipboard::Nothing
        }
    };
    if !matches!(&read, Clipboard::Image(path) if path == &png) {
        let _ = std::fs::remove_file(&png);
    }
    read
}

fn reader(png: &std::path::Path) -> Option<rimz::mux::CommandSpec> {
    let png = png.display().to_string();
    if let Some(program) = std::env::var_os(CLIPBOARD_BIN_ENV) {
        return Some(rimz::mux::CommandSpec::new(program.to_string_lossy()).arg(png));
    }
    if cfg!(target_os = "macos") {
        return Some(
            rimz::mux::CommandSpec::new("osascript")
                .args(["-l", "JavaScript", "-e", MAC_CLIPBOARD_SCRIPT])
                .arg(png),
        );
    }
    None
}

fn parse(stdout: &str) -> Clipboard {
    let mut lines = stdout.lines().filter(|line| !line.is_empty());
    let absolute = |line: &str| Some(PathBuf::from(line)).filter(|path| path.is_absolute());
    match lines.next() {
        Some("files") => {
            let paths = lines.map(absolute).collect::<Option<Vec<_>>>();
            match paths {
                Some(paths) if !paths.is_empty() => Clipboard::Files(paths),
                _ => Clipboard::Nothing,
            }
        }
        Some("image") => lines
            .next()
            .and_then(absolute)
            .map_or(Clipboard::Nothing, Clipboard::Image),
        _ => Clipboard::Nothing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reader_output_names_files_an_image_or_nothing() {
        assert_eq!(
            parse("files\n/a/b.png\n/c d/e.txt\n"),
            Clipboard::Files(vec![PathBuf::from("/a/b.png"), PathBuf::from("/c d/e.txt")])
        );
        assert_eq!(
            parse("image\n/tmp/x.png"),
            Clipboard::Image(PathBuf::from("/tmp/x.png"))
        );
        for nothing in [
            "none",
            "",
            "files\n",
            "files\nrelative.txt",
            "image\n",
            "text\n/a",
        ] {
            assert_eq!(parse(nothing), Clipboard::Nothing, "{nothing:?}");
        }
    }
}

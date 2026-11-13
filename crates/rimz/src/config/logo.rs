//! A provider's `[theme.providers.<kind>] logo` file: read under a byte cap and
//! parsed as untrusted SVG that loads nothing beyond its own bytes.

use std::io::Read;
use std::path::Path;

use super::ThemeConfig;

/// The largest logo file read. A mark is a few kilobytes; the cap bounds the
/// parse a hostile or mistaken file can ask for.
const LOGO_MAX_BYTES: u64 = 256 * 1024;

/// Read a configured logo path, `~` expanded as a theme `scheme` path is. A
/// path that is not a regular file is refused before it is opened, so a FIFO or
/// device never blocks the read.
pub(crate) fn read_logo_file(raw: &str) -> Result<Vec<u8>, String> {
    let path = super::scheme::expand_home(Path::new(raw));
    if !path.is_file() {
        return Err(format!(
            "reading provider logo `{}`: it is missing or is not a regular file",
            path.display()
        ));
    }
    let file = std::fs::File::open(&path)
        .map_err(|err| format!("reading provider logo `{}`: {err}", path.display()))?;
    let mut bytes = Vec::new();
    file.take(LOGO_MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| format!("reading provider logo `{}`: {err}", path.display()))?;
    if bytes.len() as u64 > LOGO_MAX_BYTES {
        return Err(format!(
            "provider logo `{}` is over the {} KiB cap",
            path.display(),
            LOGO_MAX_BYTES / 1024
        ));
    }
    Ok(bytes)
}

/// Parse SVG bytes with `currentColor` set to `color`. Both image-href resolvers
/// refuse, so no file, data URL, or nested SVG is loaded on the file's behalf;
/// the build carries no text or font support.
pub(crate) fn parse_logo_svg(bytes: &[u8], color: [u8; 3]) -> Result<resvg::usvg::Tree, String> {
    let [red, green, blue] = color;
    let options = resvg::usvg::Options {
        resources_dir: None,
        image_href_resolver: resvg::usvg::ImageHrefResolver {
            resolve_data: Box::new(|_, _, _| None),
            resolve_string: Box::new(|_, _| None),
        },
        style_sheet: Some(format!("svg {{ color: #{red:02x}{green:02x}{blue:02x} }}")),
        ..resvg::usvg::Options::default()
    };
    resvg::usvg::Tree::from_data(bytes, &options).map_err(|err| err.to_string())
}

/// One message per configured logo that cannot be read or parsed, naming the
/// file and the fix; the sidebar draws that provider as if `logo` were absent.
pub(super) fn validate_logos(theme: &ThemeConfig) -> Vec<String> {
    theme
        .providers
        .iter()
        .filter_map(|(kind, style)| Some((kind, style.logo.as_deref()?)))
        .filter(|(_, raw)| !raw.is_empty())
        .filter_map(|(kind, raw)| {
            let error = read_logo_file(raw)
                .and_then(|bytes| {
                    parse_logo_svg(&bytes, [0; 3]).map(|_| ()).map_err(|err| {
                        let path = super::scheme::expand_home(Path::new(raw));
                        format!("provider logo `{}` is not valid SVG: {err}", path.display())
                    })
                })
                .err()?;
            Some(format!(
                "{error}; the sidebar draws {kind} as if `[theme.providers.{kind}] logo` were \
                 unset until the file is fixed or the key removed"
            ))
        })
        .collect()
}

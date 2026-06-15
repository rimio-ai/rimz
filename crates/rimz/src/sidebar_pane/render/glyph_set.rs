use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};

use crate::config::SidebarGlyphsConfig;

const NAMED_SETS: [&str; 2] = ["unicode", "nerd-font"];
const GLYPH_TABLES: [&str; 9] = [
    "status", "cockpit", "tokens", "meter", "clock", "worktree", "card", "process", "chrome",
];

pub fn validate_glyph_source(name_or_path: &str) -> Result<(), String> {
    if is_named_glyph_set(name_or_path) {
        return Ok(());
    }
    load_external_glyphs(name_or_path).map(|_| ())
}

pub(crate) fn explicit_glyph_config(name_or_path: &str) -> Option<SidebarGlyphsConfig> {
    if is_named_glyph_set(name_or_path) {
        return None;
    }
    cached_external_glyphs(name_or_path)
}

pub(crate) fn is_named_glyph_set(name_or_path: &str) -> bool {
    NAMED_SETS.contains(&name_or_path)
}

pub fn glyph_lookup_hint() -> String {
    "named sets: unicode, nerd-font; or a path to a Rimz glyphs TOML file".to_owned()
}

fn cached_external_glyphs(name_or_path: &str) -> Option<SidebarGlyphsConfig> {
    {
        let cache = lock_explicit_glyph_cache();
        if let Some(glyphs) = cache.get(name_or_path) {
            return glyphs.clone();
        }
    }

    let glyphs = load_external_glyphs(name_or_path).ok();
    let mut cache = lock_explicit_glyph_cache();
    cache
        .entry(name_or_path.to_owned())
        .or_insert_with(|| glyphs.clone())
        .clone()
}

fn lock_explicit_glyph_cache() -> MutexGuard<'static, HashMap<String, Option<SidebarGlyphsConfig>>>
{
    static CACHED: OnceLock<Mutex<HashMap<String, Option<SidebarGlyphsConfig>>>> = OnceLock::new();
    match CACHED.get_or_init(|| Mutex::new(HashMap::new())).lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn load_external_glyphs(name_or_path: &str) -> Result<SidebarGlyphsConfig, String> {
    let path = resolve_external_glyph_path(name_or_path).ok_or_else(|| {
        format!(
            "unknown sidebar glyph set `{name_or_path}`; {}",
            glyph_lookup_hint()
        )
    })?;
    let text = std::fs::read_to_string(&path)
        .map_err(|err| format!("reading sidebar glyph set `{}`: {err}", path.display()))?;
    validate_glyph_file_tables(&text)
        .map_err(|err| format!("invalid sidebar glyph set `{}`: {err}", path.display()))?;
    let config: SidebarGlyphsConfig = toml::from_str(&text)
        .map_err(|err| format!("invalid sidebar glyph set `{}`: {err}", path.display()))?;
    validate_external_glyph_set_base(&config)
        .map_err(|err| format!("invalid sidebar glyph set `{}`: {err}", path.display()))?;
    Ok(config)
}

fn validate_glyph_file_tables(text: &str) -> Result<(), String> {
    let value: toml::Value =
        toml::from_str(text).map_err(|err| format!("parsing glyph TOML: {err}"))?;
    let Some(table) = value.as_table() else {
        return Err("glyph file must be a TOML table".to_owned());
    };
    for key in table.keys() {
        if key == "set" || GLYPH_TABLES.contains(&key.as_str()) {
            continue;
        }
        return Err(format!("unknown sidebar glyph namespace `{key}`"));
    }
    Ok(())
}

fn validate_external_glyph_set_base(config: &SidebarGlyphsConfig) -> Result<(), String> {
    let Some(set) = config.set.as_deref() else {
        return Ok(());
    };
    if is_named_glyph_set(set) {
        return Ok(());
    }
    Err(format!(
        "custom glyph file set must be `unicode` or `nerd-font`, got `{set}`"
    ))
}

fn resolve_external_glyph_path(name_or_path: &str) -> Option<PathBuf> {
    let path = expand_home(Path::new(name_or_path));
    path.is_file().then_some(path)
}

fn expand_home(path: &Path) -> PathBuf {
    let raw = path.as_os_str().to_string_lossy();
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("~"));
    }
    if let Some(stripped) = raw.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(stripped);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn named_sets_validate() {
        validate_glyph_source("unicode").expect("unicode");
        validate_glyph_source("nerd-font").expect("nerd-font");
        assert!(validate_glyph_source("auto").is_err());
    }

    #[test]
    fn custom_file_validates_known_tables_and_glyph_width() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("glyphs.toml");
        std::fs::write(&file, "[tokens]\ntotal = \"◇\"\n").expect("write glyph file");
        validate_glyph_source(file.to_str().expect("utf-8")).expect("valid glyph file");

        std::fs::write(&file, "[tokens]\ntotal = \"abc\"\n").expect("write glyph file");
        let err = validate_glyph_source(file.to_str().expect("utf-8"))
            .expect_err("over-wide glyph")
            .to_string();
        assert!(err.contains("must occupy one or two terminal cells"));

        std::fs::write(&file, "[nope]\nthing = \"x\"\n").expect("write glyph file");
        let err = validate_glyph_source(file.to_str().expect("utf-8"))
            .expect_err("unknown namespace")
            .to_string();
        assert!(err.contains("unknown sidebar glyph namespace `nope`"));
    }

    #[test]
    fn custom_file_rejects_unknown_base_set() {
        let dir = tempdir().expect("tempdir");
        let file = dir.path().join("glyphs.toml");
        std::fs::write(&file, "set = \"nerd-fnot\"\n[tokens]\ntotal = \"◇\"\n")
            .expect("write glyph file");

        let err = validate_glyph_source(file.to_str().expect("utf-8"))
            .expect_err("unknown file-local base")
            .to_string();

        assert!(
            err.contains("custom glyph file set must be `unicode` or `nerd-font`"),
            "unexpected error: {err}"
        );
    }
}

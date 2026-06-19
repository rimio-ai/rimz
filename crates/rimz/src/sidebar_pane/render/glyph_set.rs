pub fn validate_glyph_source(name: &str) -> Result<(), String> {
    if crate::config::is_named_glyph_set(name) {
        Ok(())
    } else {
        Err(format!(
            "unknown theme glyph set `{name}`; expected unicode or nerd_font"
        ))
    }
}

pub fn glyph_lookup_hint() -> String {
    "named sets: unicode, nerd_font".to_owned()
}

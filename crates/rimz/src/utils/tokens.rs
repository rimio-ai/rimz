//! Token estimates for text an agent is about to read.
//!
//! Counts use OpenAI's public `o200k_base` BPE, the current GPT tokenizer.
//! Claude's tokenizer is not published and typically counts somewhat higher,
//! so every result is an estimate and renders as one. The vocabulary is
//! embedded and parsed once, on the first estimate; the sidebar render path
//! never calls this (the elder's lost-watch fallback may, rarely).

/// Estimated token count of `text`, special-token markers read as plain text.
pub fn estimate(text: &str) -> u64 {
    tiktoken_rs::o200k_base_singleton()
        .encode_ordinary(text)
        .len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn estimate_counts_o200k_tokens() {
        assert_eq!(estimate(""), 0);
        assert_eq!(estimate("hello world"), 2);
        let code = "fn main() {\n    println!(\"{}\", 40 + 2);\n}\n";
        assert_eq!(estimate(code), estimate(code));
        assert!(estimate(code) > 5);
    }
}

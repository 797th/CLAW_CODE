//! Loss-aware prose compression for Caveman mode.

use std::collections::BTreeSet;

pub const CAVEMAN_ENV_VAR: &str = "CLAW_CAVEMAN";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CavemanMetrics {
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub original_tokens: usize,
    pub compressed_tokens: usize,
    pub important_terms: usize,
    pub retained_important_terms: usize,
}

impl CavemanMetrics {
    #[must_use]
    pub fn token_savings_percent(&self) -> f64 {
        percent_drop(self.original_tokens, self.compressed_tokens)
    }

    #[must_use]
    pub fn fidelity_percent(&self) -> f64 {
        if self.important_terms == 0 {
            return 100.0;
        }
        100.0 * self.retained_important_terms as f64 / self.important_terms as f64
    }
}

#[must_use]
pub fn caveman_enabled() -> bool {
    std::env::var(CAVEMAN_ENV_VAR).map_or(true, |value| {
        !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "off" | "normal" | "disabled"
        )
    })
}

#[must_use]
pub fn compress_caveman(text: &str) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }

    let (protected_text, protected_regions) = protect_regions(text);
    let mut compressed = String::with_capacity(protected_text.len());
    for line in protected_text.split_inclusive('\n') {
        let (content, line_ending) = line
            .strip_suffix('\n')
            .map_or((line, ""), |content| (content, "\n"));
        compressed.push_str(&compress_line(content));
        compressed.push_str(line_ending);
    }
    restore_regions(&compressed, &protected_regions)
}

#[must_use]
pub fn measure_caveman(text: &str) -> CavemanMetrics {
    let compressed = compress_caveman(text);
    let original_terms = important_terms(text);
    let compressed_terms = important_terms(&compressed);
    let retained_important_terms = original_terms
        .iter()
        .filter(|term| compressed_terms.contains(*term))
        .count();

    CavemanMetrics {
        original_chars: text.chars().count(),
        compressed_chars: compressed.chars().count(),
        original_tokens: estimate_tokens(text),
        compressed_tokens: estimate_tokens(&compressed),
        important_terms: original_terms.len(),
        retained_important_terms,
    }
}

#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    text.chars().count().div_ceil(4)
}

#[must_use]
pub fn fidelity_percent(original: &str, compressed: &str) -> f64 {
    let original_terms = important_terms(original);
    if original_terms.is_empty() {
        return 100.0;
    }
    let compressed_terms = important_terms(compressed);
    100.0
        * original_terms
            .iter()
            .filter(|term| compressed_terms.contains(*term))
            .count() as f64
        / original_terms.len() as f64
}

fn percent_drop(original: usize, compressed: usize) -> f64 {
    if original == 0 {
        return 0.0;
    }
    100.0 * original.saturating_sub(compressed) as f64 / original as f64
}

fn compress_line(line: &str) -> String {
    let Some(first_non_whitespace) = line.find(|character: char| !character.is_whitespace()) else {
        return line.to_string();
    };
    let (indent, content) = line.split_at(first_non_whitespace);

    if looks_structured(content) {
        return line.trim_end().to_string();
    }

    let words = content.split_whitespace().collect::<Vec<_>>();
    if words.is_empty() {
        return line.to_string();
    }

    let mut compacted = Vec::with_capacity(words.len());
    let mut index = 0;
    while index < words.len() {
        let key = word_key(words[index]);
        if index + 1 < words.len()
            && key == "i"
            && matches!(word_key(words[index + 1]).as_str(), "will" | "can")
        {
            index += 2;
            continue;
        }
        if index + 5 < words.len()
            && key == "i"
            && word_key(words[index + 1]) == "would"
            && word_key(words[index + 2]) == "be"
            && word_key(words[index + 3]) == "happy"
            && word_key(words[index + 4]) == "to"
            && word_key(words[index + 5]) == "help"
        {
            index += 6;
            continue;
        }
        if index + 2 < words.len()
            && key == "happy"
            && word_key(words[index + 1]) == "to"
            && word_key(words[index + 2]) == "help"
        {
            index += 3;
            continue;
        }
        if index + 1 < words.len()
            && matches!(key.as_str(), "could" | "can" | "would")
            && word_key(words[index + 1]) == "you"
        {
            index += 2;
            continue;
        }
        if is_low_signal_word(&key) {
            index += 1;
            continue;
        }
        compacted.push(words[index]);
        index += 1;
    }

    if compacted.is_empty() {
        return line.trim_end().to_string();
    }
    format!("{indent}{}", compacted.join(" "))
}

fn looks_structured(content: &str) -> bool {
    content.starts_with('{')
        || content.starts_with('[')
        || content.starts_with("<") && content.contains('>')
        || content.starts_with("diff --git ")
}

fn is_low_signal_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "the"
            | "please"
            | "kindly"
            | "just"
            | "really"
            | "very"
            | "actually"
            | "basically"
            | "simply"
            | "sure"
            | "certainly"
            | "surely"
            | "essentially"
    )
}

fn word_key(word: &str) -> String {
    word.trim_matches(|character: char| !character.is_alphanumeric())
        .to_ascii_lowercase()
}

fn important_terms(text: &str) -> BTreeSet<String> {
    let mut terms = BTreeSet::new();
    let mut current = String::new();

    for character in text.chars() {
        if character.is_alphanumeric()
            || matches!(character, '_' | '/' | '.' | ':' | '-' | '@' | '#')
        {
            current.push(character.to_ascii_lowercase());
        } else if !current.is_empty() {
            add_important_term(&mut terms, &current);
            current.clear();
        }
    }
    if !current.is_empty() {
        add_important_term(&mut terms, &current);
    }
    terms
}

fn add_important_term(terms: &mut BTreeSet<String>, term: &str) {
    let normalized = term.trim_matches(|character: char| !character.is_alphanumeric());
    if normalized.len() < 3 || is_fidelity_stop_word(normalized) {
        return;
    }
    terms.insert(normalized.to_string());
}

fn is_fidelity_stop_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "the"
            | "and"
            | "are"
            | "as"
            | "at"
            | "be"
            | "by"
            | "for"
            | "from"
            | "in"
            | "is"
            | "it"
            | "of"
            | "on"
            | "or"
            | "that"
            | "this"
            | "to"
            | "with"
            | "you"
            | "your"
            | "please"
            | "just"
            | "really"
            | "very"
            | "could"
            | "would"
            | "can"
    )
}

fn protect_regions(input: &str) -> (String, Vec<String>) {
    let mut output = String::with_capacity(input.len());
    let mut regions = Vec::new();
    let mut index = 0;

    while index < input.len() {
        let region_end = if is_fence_start(input, index) {
            fenced_region_end(input, index)
        } else if input[index..].starts_with(char::from(96)) {
            delimited_region_end(input, index, char::from(96))
        } else if is_quote_start(input, index) {
            delimited_region_end(input, index, input[index..].chars().next().unwrap())
        } else if input[index..].starts_with("http://") || input[index..].starts_with("https://") {
            input[index..]
                .find(char::is_whitespace)
                .map_or(input.len(), |offset| index + offset)
        } else {
            let character = input[index..].chars().next().unwrap();
            output.push(character);
            index += character.len_utf8();
            continue;
        };

        if region_end <= index {
            let character = input[index..].chars().next().unwrap();
            output.push(character);
            index += character.len_utf8();
            continue;
        }
        let marker = format!("\u{e000}{}\u{e001}", regions.len());
        regions.push(input[index..region_end].to_string());
        output.push_str(&marker);
        index = region_end;
    }

    (output, regions)
}

fn restore_regions(input: &str, regions: &[String]) -> String {
    let mut restored = input.to_string();
    for (index, region) in regions.iter().enumerate() {
        let marker = format!("\u{e000}{index}\u{e001}");
        restored = restored.replace(&marker, region);
    }
    restored
}

fn is_fence_start(input: &str, index: usize) -> bool {
    let line_start = input[..index]
        .rfind('\n')
        .map_or(0, |position| position + 1);
    if input[line_start..index]
        .chars()
        .any(|character| !character.is_whitespace())
    {
        return false;
    }
    input.as_bytes()[index..].starts_with(&[96, 96, 96]) || input[index..].starts_with("~~~")
}

fn fenced_region_end(input: &str, index: usize) -> usize {
    let marker = &input[index..index + 3];
    input[index + 3..]
        .find(marker)
        .map_or(input.len(), |offset| index + 3 + offset + 3)
}

fn delimited_region_end(input: &str, index: usize, delimiter: char) -> usize {
    let mut cursor = index + delimiter.len_utf8();
    while cursor < input.len() {
        let character = input[cursor..].chars().next().unwrap();
        if character == '\\' {
            cursor += character.len_utf8();
            if cursor < input.len() {
                cursor += input[cursor..].chars().next().unwrap().len_utf8();
            }
            continue;
        }
        cursor += character.len_utf8();
        if character == delimiter {
            return cursor;
        }
    }
    index
}

fn is_quote_start(input: &str, index: usize) -> bool {
    let character = input[index..].chars().next().unwrap();
    if !matches!(character, '\'' | '"') {
        return false;
    }
    if character == '\'' {
        let previous = input[..index].chars().next_back();
        if previous.is_some_and(char::is_alphanumeric) {
            return false;
        }
    }
    delimited_region_end(input, index, character) > index
}

#[cfg(test)]
mod tests {
    use super::{compress_caveman, fidelity_percent, measure_caveman};

    #[test]
    fn removes_filler_and_articles_but_keeps_constraints() {
        let source = "Please review the auth middleware and do not change the public API.";
        let compressed = compress_caveman(source);

        assert_eq!(
            compressed,
            "review auth middleware and do not change public API."
        );
        assert!(compressed.contains("do not"));
        assert!(compressed.contains("public API"));
    }

    #[test]
    fn preserves_code_quotes_urls_and_structured_lines() {
        let inline_code = char::from(96);
        let fence = inline_code.to_string().repeat(3);
        let source = format!(
            "Please update {inline_code}the API{inline_code} in src/auth/token.rs.\n\
             {fence}rust\nlet value = \"the exact text\";\n{fence}\n\
             Exact error: \"the bearer token is expired\".\n\
             https://example.test/the/path\n\
             {{\"message\": \"the exact payload\"}}\n"
        );
        let compressed = compress_caveman(&source);

        assert!(compressed.contains(&format!("{inline_code}the API{inline_code}")));
        assert!(compressed.contains("let value = \"the exact text\";"));
        assert!(compressed.contains("\"the bearer token is expired\""));
        assert!(compressed.contains("https://example.test/the/path"));
        assert!(compressed.contains("{\"message\": \"the exact payload\"}"));
        assert!(compressed.contains("src/auth/token.rs"));
    }

    #[test]
    fn handles_unclosed_inline_code_without_losing_progress() {
        let compressed = compress_caveman("Please inspect `src/auth/token.rs and preserve errors.");
        assert_eq!(
            compressed,
            "inspect `src/auth/token.rs and preserve errors."
        );
    }

    #[test]
    fn fidelity_ignores_removed_filler_but_catches_lost_technical_terms() {
        let source = "Please inspect src/auth/token.rs and preserve exact 401 errors.";
        let compressed = compress_caveman(source);
        assert_eq!(fidelity_percent(source, &compressed), 100.0);
        assert!(
            measure_caveman(source).compressed_tokens <= measure_caveman(source).original_tokens
        );
        assert!(fidelity_percent(source, "inspect auth") < 100.0);
    }
}

//! Sentence segmentation shared by the chapter renderer and the aligner input, so sentence
//! numbers in the rendered HTML always match the ones in the sync map.

/// Splits `text` into sentence byte ranges. Whitespace between sentences belongs to no sentence.
/// Deliberately simple and deterministic: a sentence ends at `.`, `!`, `?` or `…` (plus any closing
/// quotes or brackets) followed by whitespace, unless the word is a common abbreviation.
pub fn sentences(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i < chars.len() {
        let (pos, c) = chars[i];
        if start.is_none() {
            if c.is_whitespace() {
                i += 1;
                continue;
            }
            start = Some(pos);
        }
        if matches!(c, '.' | '!' | '?' | '…') {
            let mut j = i + 1;
            while j < chars.len() && matches!(chars[j].1, '.' | '!' | '?' | '…' | '"' | '\'' | '”' | '’' | ')' | ']' | '»') {
                j += 1;
            }
            let at_break = j >= chars.len() || chars[j].1.is_whitespace();
            // `"Stop!" cried Mr. Smith.` is one sentence: the next word continues it.
            let continues = chars[j..].iter().map(|&(_, c)| c).find(|c| !c.is_whitespace()).is_some_and(char::is_lowercase);
            if at_break && !continues && !(c == '.' && is_abbreviation(&text[start.unwrap()..pos])) {
                let end = if j < chars.len() { chars[j].0 } else { text.len() };
                out.push(start.unwrap()..end);
                start = None;
                i = j;
                continue;
            }
        } else if c == '\n' && i + 1 < chars.len() && chars[i + 1].1 == '\n' {
            // A blank line always ends a sentence (headings, verse, lists).
            let end = trim_end(text, start.unwrap(), pos);
            if end > start.unwrap() {
                out.push(start.unwrap()..end);
            }
            start = None;
        }
        i += 1;
    }
    if let Some(s) = start {
        let end = trim_end(text, s, text.len());
        if end > s {
            out.push(s..end);
        }
    }
    out
}

fn trim_end(text: &str, start: usize, end: usize) -> usize {
    start + text[start..end].trim_end().len()
}

fn is_abbreviation(before: &str) -> bool {
    let word = before.rsplit(|c: char| c.is_whitespace()).next().unwrap_or("");
    let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
    const ABBR: &[&str] = &[
        "Mr", "Mrs", "Ms", "Dr", "St", "Jr", "Sr", "Prof", "Capt", "Col", "Gen", "Lt", "Sgt", "Rev",
        "Hon", "vs", "etc", "e.g", "i.e", "cf", "Mt", "No", "Vol", "pp",
    ];
    // Single initials ("J. R. R. Tolkien") don't end sentences either.
    ABBR.contains(&word) || (word.chars().count() == 1 && word.chars().all(|c| c.is_uppercase()))
}

/// Lowercased words with punctuation stripped, for fuzzy title matching and alignment.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for word in s.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()) {
        if !out.is_empty() {
            out.push(' ');
        }
        out.extend(word.chars().flat_map(char::to_lowercase));
    }
    out
}

/// A key for matching an ebook to its audiobook: normalized title without leading articles,
/// subtitles or edition noise.
pub fn title_key(title: &str) -> String {
    let base = title.split([':', '(', '[']).next().unwrap_or(title);
    let n = normalize(base);
    let n = n.strip_prefix("the ").or_else(|| n.strip_prefix("a ")).or_else(|| n.strip_prefix("an ")).unwrap_or(&n);
    n.replace(" unabridged", "").replace(" a novel", "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(t: &str) -> Vec<&str> {
        sentences(t).into_iter().map(|r| &t[r]).collect()
    }

    #[test]
    fn basic() {
        assert_eq!(split("Hello there. How are you?  Fine!"), ["Hello there.", "How are you?", "Fine!"]);
    }

    #[test]
    fn quotes_and_abbreviations() {
        assert_eq!(
            split("\"Stop!\" cried Mr. Smith. Dr. J. Watson agreed."),
            ["\"Stop!\" cried Mr. Smith.", "Dr. J. Watson agreed."]
        );
    }

    #[test]
    fn blank_lines() {
        assert_eq!(split("Chapter One\n\nIt was dark"), ["Chapter One", "It was dark"]);
    }

    #[test]
    fn keys() {
        assert_eq!(title_key("The Hobbit: or There and Back Again"), "hobbit");
        assert_eq!(title_key("Hobbit (Unabridged)"), "hobbit");
    }
}

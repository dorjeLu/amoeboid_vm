//! A lexical guard run before the `kdl` crate sees the text. The crate parses blocks, nested block
//! comments and chains of `/-` recursively, using tens of kilobytes of stack per level in a debug
//! build, so a few dozen levels overflow a thread's stack and abort the process. Dicts and lists
//! need one block per level, so bounding these three bounds the conversion too.
//!
//! The scan only skips strings and line comments well enough not to count their contents; text
//! it misreads is still refused by the crate itself.

/// Deepest block, block-comment or `/-` nesting accepted.
pub const MAX_DEPTH: usize = 16;

/// `Err(offset)` of the first byte that nests deeper than [`MAX_DEPTH`].
pub(super) fn check(text: &str) -> Result<(), usize> {
    let bytes = text.as_bytes();
    let mut at = 0_usize;
    let mut blocks = 0_usize;
    let mut dashes = 0_usize;
    while let Some(&byte) = bytes.get(at) {
        let rest = bytes.get(at..).unwrap_or_default();
        if rest.starts_with(b"/-") {
            dashes = dashes.saturating_add(1);
            if dashes > MAX_DEPTH {
                return Err(at);
            }
            at = at.saturating_add(2);
            continue;
        }
        if !byte.is_ascii_whitespace() {
            dashes = 0;
        }
        at = match byte {
            b'{' => {
                blocks = blocks.saturating_add(1);
                if blocks > MAX_DEPTH {
                    return Err(at);
                }
                at.saturating_add(1)
            }
            b'}' => {
                blocks = blocks.saturating_sub(1);
                at.saturating_add(1)
            }
            b'"' | b'#' => string_end(bytes, at),
            b'/' if rest.starts_with(b"//") => line_end(bytes, at),
            b'/' if rest.starts_with(b"/*") => block_comment_end(bytes, at)?,
            _ => at.saturating_add(1),
        };
    }
    Ok(())
}

/// Offset after the line comment starting at `start`, or its newline.
fn line_end(bytes: &[u8], start: usize) -> usize {
    let rest = bytes.get(start..).unwrap_or_default();
    rest.iter()
        .position(|b| matches!(b, b'\n' | b'\r'))
        .map_or(bytes.len(), |len| start.saturating_add(len))
}

/// Offset after the (nested) block comment starting at `start`, or the end if never closed.
fn block_comment_end(bytes: &[u8], start: usize) -> Result<usize, usize> {
    let mut at = start;
    let mut depth = 0_usize;
    loop {
        let rest = bytes.get(at..).unwrap_or_default();
        if rest.starts_with(b"/*") {
            depth = depth.saturating_add(1);
            if depth > MAX_DEPTH {
                return Err(at);
            }
            at = at.saturating_add(2);
        } else if rest.starts_with(b"*/") {
            depth = depth.saturating_sub(1);
            at = at.saturating_add(2);
            if depth == 0 {
                return Ok(at);
            }
        } else if rest.is_empty() {
            return Ok(at);
        } else {
            at = at.saturating_add(1);
        }
    }
}

/// Offset after the string starting at `start` (`"`, `"""`, or either behind `#`s), or just past
/// the `#`s that turn out to open no string. A single-line string ends at a newline.
fn string_end(bytes: &[u8], start: usize) -> usize {
    let rest = bytes.get(start..).unwrap_or_default();
    let hashes = rest.iter().take_while(|b| **b == b'#').count();
    let quote = start.saturating_add(hashes);
    let after = bytes.get(quote..).unwrap_or_default();
    if !after.starts_with(b"\"") {
        return quote;
    }
    let quotes = if after.starts_with(b"\"\"\"") { 3 } else { 1 };
    let raw = hashes > 0;
    let mut at = quote.saturating_add(quotes);
    loop {
        let rest = bytes.get(at..).unwrap_or_default();
        match rest.first() {
            None => return at,
            Some(b'\\') if !raw => at = at.saturating_add(2),
            Some(b'\n') if quotes == 1 => return at,
            Some(b'"') if closes(rest, quotes, hashes) => {
                return at.saturating_add(quotes).saturating_add(hashes);
            }
            Some(_) => at = at.saturating_add(1),
        }
    }
}

/// `rest` starts with `quotes` quote marks followed by `hashes` `#`s.
fn closes(rest: &[u8], quotes: usize, hashes: usize) -> bool {
    let all = |bytes: Option<&[u8]>, wanted: u8| {
        bytes.is_some_and(|run| run.iter().all(|b| *b == wanted))
    };
    all(rest.get(..quotes), b'"') && all(rest.get(quotes..quotes.saturating_add(hashes)), b'#')
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::testing;

    #[test]
    fn closing_marks_need_the_exact_quotes_and_hashes() {
        testing::table(
            &[
                (("\"", 1, 0), true),
                (("\"x", 1, 0), true),
                (("\"\"\"", 3, 0), true),
                (("\"\"x", 3, 0), false),
                (("\"", 3, 0), false),
                (("\"#", 1, 1), true),
                (("\"##", 1, 2), true),
                (("\"#x", 1, 2), false),
                (("\"", 1, 1), false),
                (("\"\"\"##", 3, 2), true),
                (("", 1, 0), false),
            ],
            |(rest, quotes, hashes)| closes(rest.as_bytes(), *quotes, *hashes),
        );
    }

    #[test]
    fn strings_end_after_their_closing_marks() {
        testing::table(
            &[
                ("\"\"", 2),
                ("\"ab\" c", 4),
                ("\"a\\\"b\" c", 6),
                ("\"a\\\\\" c", 5),
                ("\"a{\"{", 4),
                ("\"abc", 4),
                ("\"abc\nd\"", 4),
                ("\"\"\"\na\"\"b\n\"\"\" c", 12),
                ("\"\"\"\n\\\"\"\"x\n\"\"\" c", 13),
                ("\"\"\"unclosed", 11),
                ("#\"a\\\"# c", 6),
                ("#\"a\"b\"# c", 7),
                ("##\"a\"#b\"## c", 10),
                ("#\"\"\"\na\n\"\"\"# c", 11),
                ("#true", 1),
                ("###", 3),
                ("#", 1),
                ("\"\\", 3),
            ],
            |text| string_end(text.as_bytes(), 0),
        );
    }

    #[test]
    fn line_comments_end_at_a_newline_or_the_end() {
        testing::table(
            &[
                ("// a\nb", 4),
                ("// a\rb", 4),
                ("// a", 4),
                ("//", 2),
                ("x // a{\n", 7),
            ],
            |text| line_end(text.as_bytes(), text.find("//").unwrap_or(0)),
        );
    }

    #[test]
    fn block_comments_nest_and_may_stay_open() {
        let deep = format!("{}{}", "/*".repeat(MAX_DEPTH), "*/".repeat(MAX_DEPTH));
        let too_deep = format!(
            "{}{}",
            "/*".repeat(MAX_DEPTH + 1),
            "*/".repeat(MAX_DEPTH + 1)
        );
        testing::table(
            &[
                ("/**/ a", Ok(4)),
                ("/* a */ b", Ok(7)),
                ("/* a /* b */ c */ d", Ok(17)),
                ("/* a /* b */ c", Ok(14)),
                ("/* never", Ok(8)),
                ("/*", Ok(2)),
                (deep.as_str(), Ok(4 * MAX_DEPTH)),
                (too_deep.as_str(), Err(2 * MAX_DEPTH)),
            ],
            |text| block_comment_end(text.as_bytes(), 0),
        );
    }

    #[test]
    fn nesting_up_to_max_depth_passes_and_one_more_is_located() {
        let blocks = |n: usize| format!("{}{}", "a {".repeat(n), "}".repeat(n));
        let dashes = |n: usize| format!("{}a 1", "/- ".repeat(n));
        let comments = |n: usize| format!("{}{}\na 1", "/*".repeat(n), "*/".repeat(n));
        testing::table(
            &[
                (String::new(), Ok(())),
                ("a 1\nb \"x\"\n".to_owned(), Ok(())),
                (blocks(MAX_DEPTH), Ok(())),
                (blocks(MAX_DEPTH + 1), Err(3 * MAX_DEPTH + 2)),
                (dashes(MAX_DEPTH), Ok(())),
                (dashes(MAX_DEPTH + 1), Err(3 * MAX_DEPTH)),
                (comments(MAX_DEPTH), Ok(())),
                (comments(MAX_DEPTH + 1), Err(2 * MAX_DEPTH)),
            ],
            |text| check(text),
        );
    }

    #[test]
    fn only_code_counts_not_strings_comments_or_separated_slashdashes() {
        let many_braces = "{".repeat(MAX_DEPTH + 1);
        let many_dashes = "/-".repeat(MAX_DEPTH + 1);
        testing::table(
            &[
                (format!("a \"{many_braces}\"\n"), Ok(())),
                (format!("a #\"{many_braces}\"#\n"), Ok(())),
                (format!("a \"\"\"\n{many_braces}\n\"\"\"\n"), Ok(())),
                (format!("// {many_braces}\na 1\n"), Ok(())),
                (format!("/* {many_braces} */ a 1\n"), Ok(())),
                (format!("a \"x\" // {many_dashes}\n"), Ok(())),
                (format!("a \"{many_dashes}\"\n"), Ok(())),
                // closing braces rebalance, and a stray one never goes negative
                (
                    "}}}\n".to_owned() + &"a {".repeat(MAX_DEPTH) + &"}".repeat(MAX_DEPTH),
                    Ok(()),
                ),
                // a token between slashdashes resets the chain
                (format!("{}a 1\n", "/- a 1\n".repeat(MAX_DEPTH + 1)), Ok(())),
                (
                    format!("a {{ b \"}}\" }}\n{many_braces}"),
                    Err(12 + MAX_DEPTH),
                ),
                ("#true #false 1 2\n".to_owned(), Ok(())),
                ("a/b 1 /\n".to_owned(), Ok(())),
                ("a \"unterminated\nb {}\n".to_owned(), Ok(())),
            ],
            |text| check(text),
        );
    }
}

//! `:s` with the pattern subset the development plan scopes for v0.1.
//!
//! Plan section 3.5 asks for `^ $ . * [] \ | + ?` and explicitly defers magic and
//! very-magic modes, `\zs`/`\ze`, lookaround, replacement expressions, and the
//! rest of Ex. So the pattern is that operator set in Vim's default magic mode and
//! the replacement is a literal string -- there are no groups to refer to, since
//! `(` is not in the list.
//!
//! Two points where Vim and the `regex` crate differ, both resolved here rather
//! than left to the caller:
//!
//! * `^` and `$` are line anchors in Vim, so the pattern is compiled with
//!   line-boundary matching on.
//! * `?` is literal in Vim's default magic mode. The plan lists it as supported, so
//!   it is read as magic, which is what the crate does by default.
//!
//! # Why the crate rather than a hand-written matcher
//!
//! Two reasons, the second decisive. The plan's section 5.10 already sets the
//! precedent of preferring a mature library over hand-maintained platform code. And
//! with `*` and `?` both in scope, a backtracking matcher can take exponential time
//! on a pattern like `(a*)*b` -- an editor that runs one over the contents of an
//! untrusted file would hang on it. The crate's matchers are linear-time by
//! construction.

use regex::{Regex, RegexBuilder};
use std::ops::Range;

/// One `:s` command, parsed.
#[derive(Clone, Debug)]
pub struct Substitution {
    pattern: Regex,
    replacement: String,
    global: bool,
    source: String,
}

impl Substitution {
    /// Parses `:s/pattern/replacement/[g]`.
    ///
    /// The delimiter is the first non-alphanumeric character after the `s`, as in
    /// Vim, so `:s|a|b|` works and a `|` in the pattern does not have to be escaped.
    /// A delimiter inside either half is escaped with a backslash.
    ///
    /// The error is a sentence fit for the status line, since the plan requires
    /// every error to reach the user rather than being swallowed.
    pub fn parse(command: &str) -> Result<Self, String> {
        let rest = command
            .strip_prefix('s')
            .ok_or_else(|| "not a substitution".to_string())?;
        let delimiter = rest
            .chars()
            .next()
            .filter(|c| !c.is_alphanumeric() && *c != '\\')
            .ok_or_else(|| "no delimiter after s; try :s/old/new/".to_string())?;
        if delimiter == ' ' {
            return Err("no delimiter after s; try :s/old/new/".to_string());
        }

        let body = &rest[delimiter.len_utf8()..];
        let (pattern_source, replacement_source, flags) = split_three(body, delimiter);

        if !flags.is_empty() && flags != "g" {
            return Err(format!("unsupported flag {flags:?}; only g is available"));
        }
        if pattern_source.is_empty() {
            return Err("no pattern given".to_string());
        }

        // A pattern that matches empty text is allowed, as in Vim: `s/$/!/` is an
        // ordinary command. The matcher advances past empty matches by itself, so
        // there is no risk of a loop here -- but each one becomes an edit, so a
        // degenerate pattern like `x*` over a large document produces one edit per
        // position. That is slow rather than wrong, and matches what Vim does.

        // Line-boundary matching is what makes `^` and `$` behave as Vim's do,
        // anchoring to the start and end of a line rather than of the whole text.
        let pattern = RegexBuilder::new(pattern_source)
            .multi_line(true)
            .build()
            .map_err(|error| format!("bad pattern: {}", first_line(&error.to_string())))?;
        Ok(Self {
            pattern,
            replacement: replacement_source.to_string(),
            global: flags == "g",
            source: pattern_source.to_string(),
        })
    }

    /// Builds a substitution from an already-known pattern, for callers that do
    /// not have a command string.
    pub fn from_parts(pattern: &str, replacement: &str, global: bool) -> Result<Self, String> {
        let flags = if global { "g" } else { "" };
        Self::parse(&format!("s/{pattern}/{replacement}/{flags}"))
    }

    pub fn pattern(&self) -> &str {
        &self.source
    }

    pub fn replacement(&self) -> &str {
        &self.replacement
    }

    pub fn is_global(&self) -> bool {
        self.global
    }

    /// Byte ranges of the matches, in ascending order.
    ///
    /// Zero or one match unless `g` was given, matching Vim: without it only the
    /// first occurrence on the line is replaced.
    pub fn match_ranges(&self, haystack: &str) -> Vec<Range<usize>> {
        if self.global {
            self.pattern
                .find_iter(haystack)
                .map(|m| m.range())
                .collect()
        } else {
            self.pattern
                .find(haystack)
                .map(|m| vec![m.range()])
                .unwrap_or_default()
        }
    }
}

/// Splits `body` into three halves around `delimiter`, honouring backslash escapes.
fn split_three(body: &str, delimiter: char) -> (&str, &str, &str) {
    let mut halves: [&str; 3] = [body, "", ""];
    let mut current = 0usize;
    let mut start = 0usize;
    let mut characters = body.char_indices();
    while let Some((index, character)) = characters.next() {
        if character == '\\' {
            // Skip whatever was escaped, including an escaped delimiter.
            characters.next();
            continue;
        }
        if character == delimiter && current < 2 {
            halves[current] = &body[start..index];
            current += 1;
            start = index + character.len_utf8();
        }
    }
    halves[current] = &body[start..];
    (halves[0], halves[1], halves[2])
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or(text).to_string()
}

#[cfg(test)]
mod tests {
    use super::Substitution;

    #[test]
    fn parses_a_plain_substitution() {
        let parsed = Substitution::parse("s/one/two/").unwrap();
        assert_eq!(parsed.pattern(), "one");
        assert_eq!(parsed.replacement(), "two");
        assert!(!parsed.is_global());
    }

    #[test]
    fn parses_the_global_flag() {
        let parsed = Substitution::parse("s/one/two/g").unwrap();
        assert_eq!(parsed.pattern(), "one");
        assert_eq!(parsed.replacement(), "two");
        assert!(parsed.is_global());
    }

    #[test]
    fn accepts_any_non_alphanumeric_delimiter() {
        let parsed = Substitution::parse("s|one|two|").unwrap();
        assert_eq!(parsed.pattern(), "one");
        assert_eq!(parsed.replacement(), "two");

        // A `|` inside the pattern needs no escaping when `/` is the delimiter.
        let parsed = Substitution::parse("s/a|b/c/").unwrap();
        assert_eq!(parsed.pattern(), "a|b");
    }

    #[test]
    fn an_escaped_delimiter_stays_in_the_pattern() {
        let parsed = Substitution::parse(r"s/a\/b/c/").unwrap();
        assert_eq!(parsed.pattern(), r"a\/b");
        assert_eq!(parsed.replacement(), "c");
    }

    #[test]
    fn reports_an_unparseable_command() {
        assert!(Substitution::parse("substitute/one/two/").is_err());
        assert!(Substitution::parse("s").is_err());
        assert!(Substitution::parse("s//two/").is_err());
        assert!(Substitution::parse("s/one/two/i").is_err());
    }

    #[test]
    fn reports_a_bad_pattern_as_a_sentence() {
        let error = Substitution::parse("s/(unclosed/").unwrap_err();
        assert!(error.starts_with("bad pattern:"), "got {error:?}");
        assert!(!error.contains('\n'), "the message must be one line");
    }

    #[test]
    fn rejects_a_pattern_that_matches_nothing_at_all() {
        // Vim refuses an empty pattern rather than matching every position.
        let error = Substitution::parse("s//x/").unwrap_err();
        assert_eq!(error, "no pattern given");
    }

    #[test]
    fn a_pattern_may_match_empty_text_as_in_vim() {
        // Vim allows this; `s/$/!/` is an ordinary command and must not be refused.
        let parsed = Substitution::parse("s/$/!/").unwrap();
        assert_eq!(parsed.pattern(), "$");
        assert!(!parsed.match_ranges("one").is_empty());
    }

    #[test]
    fn without_g_only_the_first_match_is_used() {
        let parsed = Substitution::parse("s/one/1/").unwrap();
        let ranges = parsed.match_ranges("one two one");
        assert_eq!(ranges.len(), 1);
        assert_eq!(ranges[0], 0..3);

        let parsed = Substitution::parse("s/one/1/g").unwrap();
        assert_eq!(parsed.match_ranges("one two one").len(), 2);
    }

    #[test]
    fn carets_are_line_anchors_as_in_vim() {
        let parsed = Substitution::parse("s/^/> /g").unwrap();
        let text = "one\ntwo\nthree";
        let hits: Vec<usize> = parsed
            .match_ranges(text)
            .iter()
            .map(|range| range.start)
            .collect();
        assert_eq!(hits, vec![0, 4, 8], "every line start should match");

        let parsed = Substitution::parse("s/$/!$/g").unwrap();
        let hits: Vec<usize> = parsed
            .match_ranges(text)
            .iter()
            .map(|range| range.start)
            .collect();
        assert_eq!(
            hits,
            vec![3, 7, 13],
            "every line end, plus the end of the text"
        );
    }

    #[test]
    fn a_dot_does_not_cross_a_line() {
        let parsed = Substitution::parse("s/a.c/X/g").unwrap();
        assert_eq!(parsed.match_ranges("abc a\nc").len(), 1);
    }

    #[test]
    fn supports_the_operator_set_the_plan_lists() {
        // * + ? | [ ]
        assert_eq!(Substitution::parse("s/ab*c/X/").unwrap().pattern(), "ab*c");
        assert_eq!(Substitution::parse("s/ab+c/X/").unwrap().pattern(), "ab+c");
        assert_eq!(Substitution::parse("s/ab?c/X/").unwrap().pattern(), "ab?c");
        assert_eq!(
            Substitution::parse("s/a(b|c)/X/").unwrap().pattern(),
            "a(b|c)"
        );
        assert_eq!(
            Substitution::parse(r"s/a\[b\]c/X/").unwrap().pattern(),
            r"a\[b\]c"
        );
    }

    #[test]
    fn the_replacement_is_literal() {
        // No groups are in scope, so `$1` and `&` are ordinary text.
        let parsed = Substitution::parse("s/a/$1&/").unwrap();
        assert_eq!(parsed.replacement(), "$1&");
    }

    #[test]
    fn matching_handles_multibyte_characters() {
        let parsed = Substitution::parse("s/世界/x/g").unwrap();
        let ranges = parsed.match_ranges("a世界b世界");
        assert_eq!(ranges.len(), 2);
        assert_eq!(ranges[0], 1..7);
        assert_eq!(ranges[1], 8..14);
    }
}

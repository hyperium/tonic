/*
 *
 * Copyright 2026 gRPC authors.
 *
 * Permission is hereby granted, free of charge, to any person obtaining a copy
 * of this software and associated documentation files (the "Software"), to
 * deal in the Software without restriction, including without limitation the
 * rights to use, copy, modify, merge, publish, distribute, sublicense, and/or
 * sell copies of the Software, and to permit persons to whom the Software is
 * furnished to do so, subject to the following conditions:
 *
 * The above copyright notice and this permission notice shall be included in
 * all copies or substantial portions of the Software.
 *
 * THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
 * IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
 * AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
 * LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING
 * FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS
 * IN THE SOFTWARE.
 *
 */

//! Validated full-string regex for `envoy.type.matcher.v3.RegexMatcher`.

use std::fmt;
use std::str::FromStr;

use regex::Regex;

const ANCHOR_PREFIX: &str = r"\A(?:";
const ANCHOR_SUFFIX: &str = r")\z";

/// A regex that can only match an entire input, as required by Envoy.
///
/// Parses a nonempty `RegexMatcher` pattern via [`FromStr`].
#[derive(Clone)]
pub(crate) struct SafeRegex(Regex);

impl FromStr for SafeRegex {
    type Err = String;

    fn from_str(pattern: &str) -> Result<Self, Self::Err> {
        if pattern.is_empty() {
            return Err("empty regex is not allowed".into());
        }
        // Validate before wrapping: an unmatched ')' must not be able to close
        // our group and introduce an unanchored alternative.
        Regex::new(pattern).map_err(|e| e.to_string())?;
        // The group binds all alternatives; \A and \z also resist multiline flags.
        Regex::new(&format!("{ANCHOR_PREFIX}{pattern}{ANCHOR_SUFFIX}"))
            .map(Self)
            .map_err(|e| e.to_string())
    }
}

impl SafeRegex {
    pub(crate) fn is_match(&self, value: &str) -> bool {
        self.0.is_match(value)
    }
}

impl fmt::Debug for SafeRegex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let anchored = self.0.as_str();
        let pattern = &anchored[ANCHOR_PREFIX.len()..anchored.len() - ANCHOR_SUFFIX.len()];
        f.debug_tuple("SafeRegex").field(&pattern).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn regex_matches_only_the_full_input() {
        for (pattern, accepted, rejected) in [
            (
                "/service/method",
                "/service/method",
                "/service/methodSuffix",
            ),
            (
                "/service/method",
                "/service/method",
                "prefix/service/method",
            ),
            ("a|ab", "ab", "abc"),
            ("a|b", "b", "xb"),
            ("a", "a", "a\n"),
            ("(?m)a", "a", "x\na\ny"),
            ("(?i)foo", "FOO", "FOOD"),
            (r"\)", ")", "))"),
            ("[)]", ")", "x)"),
            ("/caf\u{e9}/.*", "/caf\u{e9}/x", "x/caf\u{e9}/x"),
            (".*", "", "a\nb"),
        ] {
            let matcher = pattern.parse::<SafeRegex>().unwrap();
            assert!(matcher.is_match(accepted), "{pattern:?}: {accepted:?}");
            assert!(!matcher.is_match(rejected), "{pattern:?}: {rejected:?}");
        }
    }

    #[test]
    fn invalid_patterns_cannot_escape_the_anchors() {
        for pattern in ["(unclosed", "foo)|bar(?:", "foo)(?:bar", ")(?:", r")\"] {
            assert!(pattern.parse::<SafeRegex>().is_err(), "{pattern:?}");
        }
    }

    #[test]
    fn inline_comments_cannot_remove_the_anchors() {
        assert!("(?x)#".parse::<SafeRegex>().is_err());
    }

    #[test]
    fn parse_rejects_empty_patterns() {
        assert_eq!(
            "".parse::<SafeRegex>().unwrap_err(),
            "empty regex is not allowed"
        );
    }

    #[test]
    fn parse_preserves_regex_errors() {
        let pattern = "[";
        assert_eq!(
            pattern.parse::<SafeRegex>().unwrap_err(),
            Regex::new(pattern).unwrap_err().to_string()
        );
    }

    #[test]
    fn debug_reports_the_original_pattern() {
        let matcher = "/caf\u{e9}/a|/b".parse::<SafeRegex>().unwrap();
        assert_eq!(format!("{matcher:?}"), "SafeRegex(\"/caf\u{e9}/a|/b\")");
    }
}

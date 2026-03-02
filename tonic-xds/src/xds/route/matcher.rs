//! Virtual host domain matching.

use std::cmp::Reverse;

/// The wildcard used in domain matching patterns.
const WILDCARD: &str = "*";

/// Finds the best-matching virtual host for the given authority.
///
/// Per gRPC A27, domain matching priority:
/// 1. Exact match
/// 2. Suffix wildcard (`*.foo.com`)
/// 3. Prefix wildcard (`foo.*`)
/// 4. Universal wildcard `*`
///
/// Within each category, the most specific (longest non-wildcard part) wins.
/// Returns a reference to the best-matching item, or `None`.
pub(crate) fn find_best_matching_virtual_host<'a, T>(
    authority: &str,
    virtual_hosts: &'a [T],
    get_domains: impl Fn(&T) -> &[String],
) -> Option<&'a T> {
    virtual_hosts
        .iter()
        .filter_map(|vh| {
            let best_score = get_domains(vh)
                .iter()
                .filter_map(|d| match_domain(authority, d))
                .min()?;
            Some((best_score, vh))
        })
        .min_by_key(|(score, _)| *score)
        .map(|(_, vh)| vh)
}

/// How well a domain pattern matched an authority.
///
/// Sorts naturally so that better matches are smaller:
/// match type (Exact < Suffix < Prefix < Universal), then higher
/// specificity (more non-wildcard characters) breaks ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct DomainMatchScore(DomainMatchType, Reverse<usize>);

/// Domain match types ordered by priority (lower is better).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum DomainMatchType {
    Exact = 0,
    Suffix = 1,
    Prefix = 2,
    Universal = 3,
}

fn match_domain(authority: &str, pattern: &str) -> Option<DomainMatchScore> {
    if pattern == WILDCARD {
        return Some(DomainMatchScore(DomainMatchType::Universal, Reverse(0)));
    }

    let authority_lower = authority.to_ascii_lowercase();
    let pattern_lower = pattern.to_ascii_lowercase();

    if authority_lower == pattern_lower {
        return Some(DomainMatchScore(
            DomainMatchType::Exact,
            Reverse(pattern.len()),
        ));
    }

    if let Some(suffix) = pattern_lower.strip_prefix(WILDCARD) {
        if authority_lower.ends_with(suffix) && authority_lower.len() > suffix.len() {
            return Some(DomainMatchScore(
                DomainMatchType::Suffix,
                Reverse(suffix.len()),
            ));
        }
    }

    if let Some(prefix) = pattern_lower.strip_suffix(WILDCARD) {
        if authority_lower.starts_with(prefix) && authority_lower.len() > prefix.len() {
            return Some(DomainMatchScore(
                DomainMatchType::Prefix,
                Reverse(prefix.len()),
            ));
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn domains(d: &[&str]) -> Vec<String> {
        d.iter().map(|s| (*s).into()).collect()
    }

    fn find_vh<'a>(authority: &str, vhs: &'a [Vec<String>]) -> Option<&'a Vec<String>> {
        find_best_matching_virtual_host(authority, vhs, |v| v.as_slice())
    }

    #[test]
    fn domain_exact() {
        let vhs = vec![domains(&["foo.com"])];
        assert!(find_vh("foo.com", &vhs).is_some());
    }

    #[test]
    fn domain_case_insensitive() {
        let vhs = vec![domains(&["FOO.COM"])];
        assert!(find_vh("foo.com", &vhs).is_some());
    }

    #[test]
    fn domain_suffix_wildcard() {
        let vhs = vec![domains(&["*.foo.com"])];
        assert!(find_vh("bar.foo.com", &vhs).is_some());
        assert!(find_vh("foo.com", &vhs).is_none());
    }

    #[test]
    fn domain_prefix_wildcard() {
        let vhs = vec![domains(&["foo.*"])];
        assert!(find_vh("foo.bar", &vhs).is_some());
        assert!(find_vh("bar.foo", &vhs).is_none());
    }

    #[test]
    fn domain_universal() {
        let vhs = vec![domains(&["*"])];
        assert!(find_vh("anything.com", &vhs).is_some());
    }

    #[test]
    fn domain_exact_beats_suffix() {
        let vhs = vec![domains(&["*.foo.com"]), domains(&["bar.foo.com"])];
        let matched = find_vh("bar.foo.com", &vhs).unwrap();
        assert!(matched.contains(&"bar.foo.com".to_string()));
    }

    #[test]
    fn domain_suffix_beats_universal() {
        let vhs = vec![domains(&["*"]), domains(&["*.foo.com"])];
        let matched = find_vh("bar.foo.com", &vhs).unwrap();
        assert!(matched.contains(&"*.foo.com".to_string()));
    }

    #[test]
    fn domain_longer_suffix_wins() {
        let vhs = vec![domains(&["*.com"]), domains(&["*.foo.com"])];
        let matched = find_vh("bar.foo.com", &vhs).unwrap();
        assert!(matched.contains(&"*.foo.com".to_string()));
    }

    #[test]
    fn domain_no_match() {
        let vhs = vec![domains(&["foo.com"])];
        assert!(find_vh("bar.com", &vhs).is_none());
    }
}

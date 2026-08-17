//! Vocabulary-neutral handling for exact unit terms.
//!
//! This module deliberately has no spelling aliases, unit catalogue, or
//! quantity classification. Population keeps the term chosen by the reader
//! or active ontology exactly; callers that need semantic interpretation must
//! obtain it from that ontology rather than from Rust constants.

use crate::UnitTerm;

/// Whether an exact unit term occurs as a complete lexical term in `span`.
#[must_use]
pub fn span_contains_resolved_unit(span: &str, expected: &UnitTerm) -> bool {
    span.match_indices(expected.as_str())
        .any(|(start, term)| has_boundaries(span, start, start + term.len(), term))
}

/// Whether `expected` begins immediately after a numeric lexeme, separated
/// only by whitespace.
#[must_use]
pub fn span_value_has_resolved_unit(span: &str, value_end: usize, expected: &UnitTerm) -> bool {
    if value_end > span.len() || !span.is_char_boundary(value_end) {
        return false;
    }
    let whitespace = span[value_end..]
        .char_indices()
        .take_while(|(_, character)| character.is_whitespace())
        .map(|(offset, character)| offset + character.len_utf8())
        .last()
        .unwrap_or(0);
    let start = value_end + whitespace;
    span[start..].starts_with(expected.as_str())
        && has_boundaries(
            span,
            start,
            start + expected.as_str().len(),
            expected.as_str(),
        )
}

fn has_boundaries(span: &str, start: usize, end: usize, term: &str) -> bool {
    let first = term.chars().next();
    let last = term.chars().next_back();
    let left_ok = span[..start]
        .chars()
        .next_back()
        .zip(first)
        .is_none_or(|(left, first)| !is_word(left) || !is_word(first));
    let right_ok = span[end..]
        .chars()
        .next()
        .zip(last)
        .is_none_or(|(right, last)| !is_word(right) || !is_word(last));
    left_ok && right_ok
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_term_accepts_exact_nonempty_terms_without_a_vocabulary() {
        // CONTRACT CHANGE: the old test enumerated accepted unit spellings.
        // The only structural rule now is non-emptiness, and no spelling is
        // rewritten behind the reader's back.
        for term in [
            "source-unit",
            "customer:unit",
            "https://example.test/ontology/unit/one",
            "source:unit-µ",
        ] {
            assert_eq!(UnitTerm::new(term).unwrap().as_str(), term);
        }
        assert!(UnitTerm::new("").is_err());
        assert!(UnitTerm::new("   ").is_err());
    }

    #[test]
    fn exact_source_matching_has_no_alias_table() {
        // CONTRACT CHANGE: source evidence now confirms the exact stored term;
        // Rust no longer claims two different spellings mean the same unit.
        let term = UnitTerm::new("customer:unit").unwrap();
        let span = "value 12 customer:unit; another 12 source-unit";
        assert!(span_contains_resolved_unit(span, &term));
        assert!(span_value_has_resolved_unit(span, 8, &term));
        assert!(!span_value_has_resolved_unit(
            span,
            8,
            &UnitTerm::new("source-unit").unwrap()
        ));
    }
}

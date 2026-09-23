//! Payment-card (PAN) detection and redaction.
//!
//! Detection and redaction share one candidate regex and one predicate so a
//! number the classifier flags as a card is always one the redactor scrubs.

use std::sync::OnceLock;

use regex::Regex;

fn credit_card_candidate_regex() -> &'static Regex {
    // 13–19 digit runs with optional single-space or single-dash
    // separators. Word boundaries keep us from matching inside larger
    // alphanumeric blobs (UUIDs, base64, etc.), and the Luhn check at
    // the call site filters out unrelated runs (phone numbers, ISBNs).
    static CC_CANDIDATE: OnceLock<Regex> = OnceLock::new();
    CC_CANDIDATE.get_or_init(|| {
        Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").expect("credit-card regex compiles")
    })
}

pub(super) fn redact_credit_cards(text: &str) -> String {
    credit_card_candidate_regex()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let matched = &caps[0];
            if is_luhn_pan(matched) {
                "[REDACTED]".to_owned()
            } else {
                matched.to_owned()
            }
        })
        .into_owned()
}

/// True when `matched` — a digit run from `credit_card_candidate_regex`,
/// possibly carrying single space/dash separators — is a 13–19 digit
/// Luhn-valid PAN.
///
/// Shared by detection (`contains_credit_card`) and redaction
/// (`redact_credit_cards`) so the two can never drift: a candidate the
/// detector flags as a card is always one the redactor scrubs. Keeping the
/// digit-length range and Luhn check in one place removes the risk of editing
/// one side (e.g. the `13..=19` bound) and silently leaving a detected card in
/// plaintext.
fn is_luhn_pan(matched: &str) -> bool {
    let digits: String = matched.chars().filter(char::is_ascii_digit).collect();
    (13..=19).contains(&digits.len()) && luhn_valid(&digits)
}

pub(super) fn contains_credit_card(text: &str) -> bool {
    // Detection runs candidate-by-candidate (rather than collapsing every
    // digit in the body) so a clip that pairs a PAN with adjacent expiry /
    // CVV digits still classifies as Secret. Earlier whole-string Luhn made
    // `4111 1111 1111 1111 exp 12/30 cvv 123` come out Public — the raw
    // PAN then bypassed `apply_secret_handling` and landed on disk.
    credit_card_candidate_regex()
        .find_iter(text)
        .any(|m| is_luhn_pan(m.as_str()))
}

pub(super) fn luhn_valid(digits: &str) -> bool {
    let mut sum = 0;
    let mut double = false;
    for ch in digits.chars().rev() {
        let Some(mut digit) = ch.to_digit(10) else {
            return false;
        };
        if double {
            digit *= 2;
            if digit > 9 {
                digit -= 9;
            }
        }
        sum += digit;
        double = !double;
    }
    sum % 10 == 0
}

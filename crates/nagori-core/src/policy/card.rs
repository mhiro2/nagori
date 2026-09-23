//! Payment-card (PAN) detection and redaction.
//!
//! Detection and redaction share one candidate regex and one predicate so a
//! number the classifier flags as a card is always one the redactor scrubs.

use std::sync::OnceLock;

use regex::Regex;

fn credit_card_candidate_regex() -> &'static Regex {
    // 13–19 digit runs with optional single-space or single-dash
    // separators. Word boundaries keep us from matching inside larger
    // alphanumeric blobs (UUIDs, base64, etc.), and `is_probable_pan` at
    // the call site filters out unrelated runs (phone numbers, ISBNs,
    // timestamps, IDs).
    static CC_CANDIDATE: OnceLock<Regex> = OnceLock::new();
    CC_CANDIDATE.get_or_init(|| {
        Regex::new(r"\b\d(?:[ -]?\d){12,18}\b").expect("credit-card regex compiles")
    })
}

/// Replace every card number in `text` with a masked marker that keeps only
/// the last four digits, e.g. `[REDACTED ••••1111]`.
///
/// The last four are what receipts and card-management screens show, so
/// keeping them discloses nothing a cardholder would not already print, but
/// it makes the marker information-bearing. That matters for a clip that is
/// *only* a card number: a bare `[REDACTED]` body counts as fully redacted
/// and is refused storage, which silently lost any number mistaken for a
/// card, whereas the masked marker persists as a row the user can see and
/// delete, and cards with different last four digits no longer dedup into
/// one row (two cards that share them still do).
pub(super) fn redact_credit_cards(text: &str) -> String {
    credit_card_candidate_regex()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let matched = &caps[0];
            if is_probable_pan(matched) {
                masked_pan(matched)
            } else {
                matched.to_owned()
            }
        })
        .into_owned()
}

fn masked_pan(matched: &str) -> String {
    let digits: Vec<char> = matched.chars().filter(char::is_ascii_digit).collect();
    let last_four: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("[REDACTED ••••{last_four}]")
}

/// True when `matched` — a digit run from `credit_card_candidate_regex`,
/// possibly carrying single space/dash separators — is shaped like a real
/// PAN: its issuer prefix and length pair up with a card network's published
/// range, and it passes Luhn.
///
/// Luhn alone is far too weak a filter: it holds for roughly one in ten
/// random digit strings, so on its own it flagged about a tenth of all
/// 13–19 digit numbers — epoch-millisecond timestamps, snowflake IDs, order
/// numbers — as cards, and a clip made of nothing but such a number was then
/// refused storage. Requiring an issuer prefix rules out the bulk of those
/// before Luhn runs: timestamps and snowflake IDs lead with `1`, which only
/// UATP issues from, and only at 15 digits.
///
/// Shared by detection (`contains_credit_card`) and redaction
/// (`redact_credit_cards`) so the two can never drift: a candidate the
/// detector flags as a card is always one the redactor scrubs. Keeping the
/// prefix / length table and the Luhn check in one place removes the risk of
/// editing one side and silently leaving a detected card in plaintext.
fn is_probable_pan(matched: &str) -> bool {
    let digits: String = matched.chars().filter(char::is_ascii_digit).collect();
    matches_issuer_range(&digits) && luhn_valid(&digits)
}

/// One card network's issuer-prefix range and the PAN lengths it issues.
struct IssuerRange {
    /// How many leading digits `low` / `high` describe.
    prefix_len: usize,
    low: u32,
    high: u32,
    lengths: &'static [usize],
}

const fn issuer(prefix_len: usize, low: u32, high: u32, lengths: &'static [usize]) -> IssuerRange {
    IssuerRange {
        prefix_len,
        low,
        high,
        lengths,
    }
}

const LEN_16: &[usize] = &[16];
const LEN_13_TO_19: &[usize] = &[13, 14, 15, 16, 17, 18, 19];
const LEN_16_TO_19: &[usize] = &[16, 17, 18, 19];

/// Issuer prefix ranges of the card networks in wide circulation, with the
/// PAN lengths each issues. Deliberately a list of networks rather than a
/// blanket "starts with 2–6" rule: every prefix it leaves out is a class of
/// ordinary numbers (timestamps, IDs) that no longer gets mistaken for a
/// card.
const ISSUER_RANGES: &[IssuerRange] = &[
    // Visa.
    issuer(1, 4, 4, &[13, 16, 19]),
    // Mastercard, including the 2-series.
    issuer(2, 51, 55, LEN_16),
    issuer(4, 2221, 2720, LEN_16),
    // American Express.
    issuer(2, 34, 34, &[15]),
    issuer(2, 37, 37, &[15]),
    // Diners Club, plus the 3-series ranges Discover Global Network
    // publishes for its network partners.
    // Diners' 300–305 keeps its legacy 14-digit cards alongside the 16–19
    // digit ones the network issues today.
    issuer(3, 300, 305, &[14, 16, 17, 18, 19]),
    issuer(4, 3095, 3095, LEN_16_TO_19),
    issuer(2, 36, 36, &[14, 15, 16]),
    issuer(2, 38, 39, LEN_16_TO_19),
    issuer(4, 3088, 3094, LEN_16_TO_19),
    issuer(4, 3096, 3102, LEN_16_TO_19),
    issuer(4, 3112, 3120, LEN_16_TO_19),
    issuer(4, 3158, 3159, LEN_16_TO_19),
    issuer(4, 3337, 3349, LEN_16_TO_19),
    // JCB.
    issuer(4, 3528, 3589, LEN_16_TO_19),
    // Discover.
    issuer(4, 6011, 6011, LEN_16_TO_19),
    issuer(3, 644, 649, LEN_16_TO_19),
    issuer(2, 65, 65, LEN_16_TO_19),
    // UnionPay.
    issuer(2, 62, 62, LEN_16_TO_19),
    // Mir.
    issuer(4, 2200, 2204, LEN_16_TO_19),
    // Maestro (the ranges still issued; 12-digit PANs sit below the
    // candidate regex's 13-digit floor).
    issuer(4, 5018, 5018, LEN_13_TO_19),
    issuer(4, 5020, 5020, LEN_13_TO_19),
    issuer(4, 5038, 5038, LEN_13_TO_19),
    issuer(4, 5893, 5893, LEN_13_TO_19),
    issuer(4, 6304, 6304, LEN_13_TO_19),
    issuer(4, 6759, 6759, LEN_13_TO_19),
    issuer(4, 6761, 6763, LEN_13_TO_19),
    // RuPay.
    issuer(2, 60, 60, LEN_16),
    issuer(4, 8100, 8171, LEN_16_TO_19),
    issuer(2, 82, 82, LEN_16),
    issuer(3, 508, 508, LEN_16),
    // Verve.
    issuer(6, 506_099, 506_198, &[16, 18, 19]),
    // Troy.
    issuer(4, 9792, 9792, LEN_16),
    // UATP (airline cards). The only network issuing from `1`, and only at
    // 15 digits, so 13-digit millisecond timestamps and 18–19 digit
    // snowflake IDs still fall outside every range.
    issuer(1, 1, 1, &[15]),
];

/// Whether `digits` (separators already stripped) has a length and leading
/// digits that some network in [`ISSUER_RANGES`] issues.
fn matches_issuer_range(digits: &str) -> bool {
    ISSUER_RANGES.iter().any(|range| {
        range.lengths.contains(&digits.len())
            && digits
                .get(..range.prefix_len)
                .and_then(|prefix| prefix.parse::<u32>().ok())
                .is_some_and(|prefix| (range.low..=range.high).contains(&prefix))
    })
}

pub(super) fn contains_credit_card(text: &str) -> bool {
    // Detection runs candidate-by-candidate (rather than collapsing every
    // digit in the body) so a clip that pairs a PAN with adjacent expiry /
    // CVV digits still classifies as Secret. Earlier whole-string Luhn made
    // `4111 1111 1111 1111 exp 12/30 cvv 123` come out Public — the raw
    // PAN then bypassed `apply_secret_handling` and landed on disk.
    credit_card_candidate_regex()
        .find_iter(text)
        .any(|m| is_probable_pan(m.as_str()))
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

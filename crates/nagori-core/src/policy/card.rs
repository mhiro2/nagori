//! Payment-card (PAN) detection and redaction.
//!
//! Detection and redaction share one scanner and one predicate so a number
//! the classifier flags as a card is always one the redactor scrubs.

use std::ops::Range;

const MIN_PAN_DIGITS: usize = 13;
const MAX_PAN_DIGITS: usize = 19;

/// Byte ranges of the card numbers in `text`, found lazily so a caller that
/// only needs to know whether there is one can stop at the first.
///
/// A candidate is a 13–19 digit run with optional single-space or
/// single-dash separators that is not glued to further digits. Only digits
/// delimit it — not word boundaries — so a number written straight after
/// letters or CJK text (`PAN4111…`, `カード4111…です`) is still found.
/// From each run start the longest candidate that passes `is_probable_pan`
/// wins, falling back to shorter ones, so digits trailing a card after a
/// separator (`4111 1111 1111 1111 123`) cannot hide it.
fn card_spans(text: &str) -> impl Iterator<Item = Range<usize>> + '_ {
    let bytes = text.as_bytes();
    let mut start = 0;
    std::iter::from_fn(move || {
        while start < bytes.len() {
            let at_run_start =
                bytes[start].is_ascii_digit() && (start == 0 || !bytes[start - 1].is_ascii_digit());
            if at_run_start && let Some(end) = longest_pan_from(bytes, start) {
                let span = start..end;
                start = end;
                return Some(span);
            }
            start += 1;
        }
        None
    })
}

/// End of the longest card number starting at `start` (a digit), if any.
/// Candidate bounds always fall on ASCII bytes, so the returned range slices
/// the text on char boundaries.
fn longest_pan_from(bytes: &[u8], start: usize) -> Option<usize> {
    let mut digits = [0_u8; MAX_PAN_DIGITS];
    // (end offset, digit count) of each candidate, shortest first.
    let mut ends = [(0_usize, 0_usize); MAX_PAN_DIGITS - MIN_PAN_DIGITS + 1];
    let mut end_count = 0;
    let mut count = 0;
    let mut pos = start;
    while count < MAX_PAN_DIGITS {
        // `bytes[pos]` is a digit here.
        digits[count] = bytes[pos];
        count += 1;
        pos += 1;
        let next = bytes.get(pos).copied();
        if count >= MIN_PAN_DIGITS && !next.is_some_and(|b| b.is_ascii_digit()) {
            ends[end_count] = (pos, count);
            end_count += 1;
        }
        match next {
            Some(b) if b.is_ascii_digit() => {}
            Some(b' ' | b'-') if bytes.get(pos + 1).is_some_and(u8::is_ascii_digit) => pos += 1,
            _ => break,
        }
    }
    // Only ASCII digits were copied in, so this never fails.
    let scanned = std::str::from_utf8(&digits[..count]).ok()?;
    // Every candidate from this start shares its leading digits, so look the
    // issuer up once rather than per candidate length.
    let issued = issued_lengths(scanned);
    ends[..end_count]
        .iter()
        .rev()
        .find(|&&(_, count)| is_probable_pan(&scanned[..count], issued))
        .map(|&(end, _)| end)
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
    let mut redacted = String::with_capacity(text.len());
    let mut last = 0;
    for span in card_spans(text) {
        redacted.push_str(&text[last..span.start]);
        redacted.push_str(&masked_pan(&text[span.clone()]));
        last = span.end;
    }
    redacted.push_str(&text[last..]);
    redacted
}

fn masked_pan(matched: &str) -> String {
    let digits: Vec<char> = matched.chars().filter(char::is_ascii_digit).collect();
    let last_four: String = digits[digits.len().saturating_sub(4)..].iter().collect();
    format!("[REDACTED ••••{last_four}]")
}

/// True when `digits` — a candidate from `card_spans` with its separators
/// stripped — is shaped like a real PAN: its length is one that
/// `issued` (the [`issued_lengths`] of its leading digits) allows, and it
/// passes Luhn.
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
fn is_probable_pan(digits: &str, issued: u32) -> bool {
    issued & (1 << digits.len()) != 0 && luhn_valid(digits)
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
    // scanner's 13-digit floor).
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

/// Bitmask (bit `n` set = length `n`) of the PAN lengths that the networks
/// in [`ISSUER_RANGES`] issue for the leading digits of `digits`; `0` when
/// no network issues from them.
fn issued_lengths(digits: &str) -> u32 {
    ISSUER_RANGES
        .iter()
        .filter(|range| {
            digits
                .get(..range.prefix_len)
                .and_then(|prefix| prefix.parse::<u32>().ok())
                .is_some_and(|prefix| (range.low..=range.high).contains(&prefix))
        })
        .flat_map(|range| range.lengths)
        .fold(0, |mask, &len| mask | (1 << len))
}

pub(super) fn contains_credit_card(text: &str) -> bool {
    // Detection runs candidate-by-candidate (rather than collapsing every
    // digit in the body) so a clip that pairs a PAN with adjacent expiry /
    // CVV digits still classifies as Secret. Earlier whole-string Luhn made
    // `4111 1111 1111 1111 exp 12/30 cvv 123` come out Public — the raw
    // PAN then bypassed `apply_secret_handling` and landed on disk.
    card_spans(text).next().is_some()
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

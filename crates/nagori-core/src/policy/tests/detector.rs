use crate::EntryFactory;

use super::super::*;
use super::{SAMPLE_PRIVATE_KEY, TEST_CREDIT_CARDS};

fn classify_default(text: &str) -> SensitivityClassification {
    let entry = EntryFactory::from_text(text);
    SensitivityClassifier::try_new(AppSettings::default())
        .unwrap()
        .classify(&entry)
}

#[test]
fn phone_numbers_do_not_trigger_otp_or_cc() {
    // Mix of formats that show up on clipboards. None of these should be
    // flagged as OTP (6–8 pure ascii digits) or as a Luhn-valid CC.
    let phones = [
        "+1 (555) 123-4567",
        "555-123-4567",
        "(555) 123-4567",
        "+81 90-1234-5678",
        "090-1234-5678",
        "+44 20 7946 0958",
        "+33 1 23 45 67 89",
        "(03) 1234-5678",
        "Call me at 555.123.4567 tomorrow",
        "Dial 1-800-555-0199 for support",
    ];
    for phone in phones {
        let result = classify_default(phone);
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::OneTimePasswordPattern),
            "phone {phone:?} was misclassified as OTP",
        );
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::CreditCardPattern),
            "phone {phone:?} was misclassified as credit card",
        );
        assert_eq!(
            result.sensitivity,
            Sensitivity::Public,
            "phone {phone:?} should remain Public, got {:?} ({:?})",
            result.sensitivity,
            result.reasons,
        );
    }
}

#[test]
fn addresses_do_not_trigger_otp_or_cc() {
    let addresses = [
        "1600 Pennsylvania Ave NW, Washington, DC 20500",
        "350 Fifth Avenue, New York, NY 10118",
        "1 Infinite Loop, Cupertino, CA 95014",
        "東京都千代田区千代田1-1",
        "〒100-0001 東京都千代田区千代田1-1",
        "Postcode SW1A 1AA, London",
        "10 Downing Street, London SW1A 2AA",
    ];
    for addr in addresses {
        let result = classify_default(addr);
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::OneTimePasswordPattern),
            "address {addr:?} was misclassified as OTP",
        );
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::CreditCardPattern),
            "address {addr:?} was misclassified as credit card",
        );
        assert_eq!(
            result.sensitivity,
            Sensitivity::Public,
            "address {addr:?} should remain Public, got {:?} ({:?})",
            result.sensitivity,
            result.reasons,
        );
    }
}

#[test]
fn ordinary_text_is_not_flagged() {
    let samples = [
        "Hello, world!",
        "The quick brown fox jumps over the lazy dog.",
        "クリップボード履歴のテスト",
        "Order #12345 ships next Tuesday at 14:30.",
        "Meeting room 4F-201, building A",
        "https://example.com/article?id=42",
        "TODO: refactor the search ranker",
        "Total: $19.99 (incl. tax)",
        "ISO 8601: 2026-05-05T12:00:00Z",
        "Lorem ipsum dolor sit amet, consectetur adipiscing elit.",
    ];
    for sample in samples {
        let result = classify_default(sample);
        assert_eq!(
            result.sensitivity,
            Sensitivity::Public,
            "sample {sample:?} should remain Public, got {:?} ({:?})",
            result.sensitivity,
            result.reasons,
        );
        assert!(
            result.reasons.is_empty(),
            "sample {sample:?} should have no reasons, got {:?}",
            result.reasons,
        );
    }
}

#[test]
fn otp_boundary_lengths_are_respected() {
    // Pure-digit strings outside the 6..=8 window must not be flagged
    // as OTP, while values within the window remain flagged.
    for len in [3_usize, 4, 5, 9, 10, 11, 12] {
        let text: String = std::iter::repeat_n('1', len).collect();
        let result = classify_default(&text);
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::OneTimePasswordPattern),
            "len {len} digit string {text:?} should not be OTP",
        );
    }
    for len in [6_usize, 7, 8] {
        let text: String = std::iter::repeat_n('1', len).collect();
        let result = classify_default(&text);
        assert!(
            result
                .reasons
                .contains(&SensitivityReason::OneTimePasswordPattern),
            "len {len} digit string {text:?} should be OTP",
        );
    }
}

#[test]
fn luhn_invalid_long_digit_runs_are_not_credit_cards() {
    // 13–19 digit strings that fail the Luhn check (e.g. simple
    // sequences and repeated digits) must stay Public.
    let candidates = [
        "1111111111111",       // 13 × 1
        "1234567890123",       // 13 digits, fails Luhn
        "1234567890123456",    // 16 digits, fails Luhn
        "9999999999999999",    // 16 × 9, fails Luhn
        "1234567890123456789", // 19 digits, fails Luhn
    ];
    for digits in candidates {
        assert!(
            !luhn_valid(digits),
            "test premise: {digits} should fail Luhn"
        );
        let result = classify_default(digits);
        assert!(
            !result
                .reasons
                .contains(&SensitivityReason::CreditCardPattern),
            "{digits:?} luhn-invalid run should not be credit card",
        );
    }
}

#[test]
fn classifies_luhn_valid_credit_cards_as_secret() {
    for pan in TEST_CREDIT_CARDS {
        let result = classify_default(pan);
        assert_eq!(
            result.sensitivity,
            Sensitivity::Secret,
            "{pan:?} should classify as Secret, got {:?} ({:?})",
            result.sensitivity,
            result.reasons,
        );
        assert!(
            result
                .reasons
                .contains(&SensitivityReason::CreditCardPattern),
            "{pan:?} should report CreditCardPattern, got {:?}",
            result.reasons,
        );
    }
}

#[test]
fn classifies_private_key_blob_as_secret() {
    let result = classify_default(SAMPLE_PRIVATE_KEY);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::PrivateKeyPattern),
        "expected PrivateKeyPattern reason, got {:?}",
        result.reasons,
    );
}

#[test]
fn classifies_credit_card_with_adjacent_expiry_and_cvv_as_secret() {
    // Earlier the classifier collapsed every digit in the body into one
    // Luhn check, so "4111 1111 1111 1111 exp 12/30 cvv 123" had 22
    // digits and failed the 13–19 length window — even though a real
    // PAN was sitting at the front. Candidate-level Luhn promotes the
    // clip to Secret so it goes through `StoreRedacted`.
    let entry = EntryFactory::from_text("card 4111 1111 1111 1111 exp 12/30 cvv 123");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(
        result.sensitivity,
        Sensitivity::Secret,
        "PAN-with-extras must classify as Secret, got {:?} ({:?})",
        result.sensitivity,
        result.reasons,
    );
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::CreditCardPattern),
        "expected CreditCardPattern, got {:?}",
        result.reasons,
    );
}

use crate::EntryFactory;

use super::super::*;
use super::{SAMPLE_PRIVATE_KEY, TEST_CREDIT_CARDS};

#[test]
fn redacts_common_secret_patterns() {
    let redacted = redact_text("api_key = abcdefghijk and token ghp_abcdefghijklmnopqrstuvwxyz");

    assert_eq!(redacted, "[REDACTED] and token [REDACTED]");
}

#[test]
fn redacts_private_key_block() {
    // A PEM block detected by `contains_private_key` must not survive in
    // the redacted body. Regression for the gap where Secret entries
    // classified by PrivateKeyPattern still kept the raw key on disk
    // under the default `StoreRedacted` policy.
    let body = "intro\n-----BEGIN OPENSSH PRIVATE KEY-----\nABCDEFG\nHIJKLMN\n-----END OPENSSH PRIVATE KEY-----\noutro";
    let redacted = redact_text(body);
    assert!(
        !redacted.contains("ABCDEFG"),
        "raw key body leaked: {redacted:?}"
    );
    assert!(redacted.contains("[REDACTED]"));
    assert!(redacted.starts_with("intro\n"));
    assert!(redacted.ends_with("\noutro"));
}

#[test]
fn redacts_truncated_private_key_block() {
    // `contains_private_key` only requires both `-----BEGIN` and
    // `PRIVATE KEY-----` to appear, so a truncated PEM (no END marker)
    // is still classified as Secret. Redaction must zap the whole tail
    // — leaving the body exposed would betray the user-visible "secret"
    // tag.
    let body = "before\n-----BEGIN RSA PRIVATE KEY-----\nrawbase64body";
    let redacted = redact_text(body);
    assert!(
        !redacted.contains("rawbase64body"),
        "truncated key leaked: {redacted:?}"
    );
    assert!(redacted.contains("[REDACTED]"));
}

#[test]
fn redacts_luhn_valid_credit_card() {
    // 4111 1111 1111 1111 is the canonical Visa test number — Luhn-valid,
    // 16 digits — and must not survive the redaction pass.
    let cases = [
        "card 4111 1111 1111 1111 expires soon",
        "card 4111-1111-1111-1111 expires soon",
        "card 4111111111111111 expires soon",
    ];
    for case in cases {
        let redacted = redact_text(case);
        assert!(
            !redacted.contains("4111"),
            "credit card leaked from {case:?}: {redacted:?}",
        );
        assert!(redacted.contains("[REDACTED]"));
    }
}

#[test]
fn redact_text_preserves_phone_numbers() {
    // Phone numbers are 10–11 digits and Luhn-invalid runs of 13–19
    // digits must not be redacted — that would mangle ordinary text
    // (order numbers, ISBNs, …) classified as Public.
    let phone = "Call me at +1 (555) 123-4567 tomorrow";
    let redacted = redact_text(phone);
    assert_eq!(redacted, phone);
}

#[test]
fn redacts_otp_when_body_is_only_digits() {
    // OTP detection looks at the entire trimmed body, so redaction must
    // mirror that: only when the clip *is* the OTP. Bare-prose digits
    // mid-sentence stay untouched (covered by the phone-number test).
    for code in ["123456", "1234567", "12345678", "  654321\n"] {
        let redacted = redact_text(code);
        assert!(
            !redacted.contains("123456")
                && !redacted.contains("1234567")
                && !redacted.contains("12345678")
                && !redacted.contains("654321"),
            "OTP {code:?} leaked into preview: {redacted:?}",
        );
        assert!(redacted.contains("[REDACTED]"));
    }
}

#[test]
fn store_redacted_strips_private_key_from_persisted_body() {
    // Default `StoreRedacted` must rewrite the durable body for a PEM
    // private key — the classifier flags it as Secret, but before this
    // change `redact_text` had no rule for `-----BEGIN … PRIVATE KEY-----`
    // and the raw key landed in SQLite verbatim.
    let raw = "intro\n-----BEGIN OPENSSH PRIVATE KEY-----\nABCDEFG\nHIJKLMN\n-----END OPENSSH PRIVATE KEY-----\noutro";
    let mut entry = EntryFactory::from_text(raw);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);

    assert_eq!(action, SecretAction::Persist);
    let body = entry.plain_text().expect("redacted body").to_owned();
    assert!(
        !body.contains("ABCDEFG"),
        "private key body leaked into stored entry: {body:?}",
    );
    assert!(body.contains("[REDACTED]"));
}

#[test]
fn store_redacted_strips_credit_card_from_persisted_body() {
    let raw = "card 4111 1111 1111 1111 expires soon";
    let mut entry = EntryFactory::from_text(raw);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);

    assert_eq!(action, SecretAction::Persist);
    let body = entry.plain_text().expect("redacted body").to_owned();
    assert!(
        !body.contains("4111"),
        "credit card digits leaked into stored entry: {body:?}",
    );
    assert!(body.contains("[REDACTED]"));
}

#[test]
fn classifier_redact_applies_user_regexes() {
    // The redacted preview must run through user regex patterns too —
    // otherwise users who configure a private prefix (e.g. INTERNAL-…)
    // see the raw value in previews even though it triggered a Private
    // classification.
    let settings = AppSettings {
        regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
        ..Default::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let redacted = classifier.redact("see ticket INTERNAL-42 for context");
    assert_eq!(redacted, "see ticket [REDACTED] for context");
}

#[test]
fn apply_secret_handling_block_returns_drop() {
    let mut entry = EntryFactory::from_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::Block);
    assert_eq!(action, SecretAction::Drop);
    // Block must not mutate the entry — caller is responsible for
    // throwing it away. Asserting the body stayed put guards against a
    // future refactor that accidentally redacts before the drop.
    assert_eq!(
        entry.plain_text(),
        Some("token = ghp_abcdefghijklmnopqrstuvwxyz123456"),
    );
}

#[test]
fn apply_secret_handling_store_redacted_rewrites_body() {
    let raw = "token = ghp_abcdefghijklmnopqrstuvwxyz123456";
    let mut entry = EntryFactory::from_text(raw);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    let original_hash = entry.metadata.content_hash.value.clone();

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);

    assert_eq!(action, SecretAction::Persist);
    let redacted_text = entry.plain_text().expect("redacted body").to_owned();
    assert!(
        !redacted_text.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "raw token must not survive in stored body, got: {redacted_text:?}",
    );
    assert!(redacted_text.contains("[REDACTED]"));
    // The content hash must reflect the redacted body so dedup keys off
    // what's actually persisted, not the raw secret.
    assert_ne!(
        entry.metadata.content_hash.value, original_hash,
        "content hash must be recomputed for the redacted body",
    );
    // Search must agree with the durable copy: normalized_text and the
    // tokens index both have to drop the raw secret too.
    assert!(
        !entry.search.normalized_text.contains("ghp_"),
        "normalized_text leaked the raw secret: {:?}",
        entry.search.normalized_text,
    );
    assert!(
        !entry
            .search
            .tokens
            .iter()
            .any(|tok| tok.contains("ghp_abcdefghijklmnopqrstuvwxyz123456")),
        "tokens index leaked the raw secret: {:?}",
        entry.search.tokens,
    );
    // The search preview must be redacted too. The daemon path overwrites
    // it before calling this, but the core API must be self-contained so a
    // direct caller can't persist a raw-secret preview.
    assert!(
        !entry
            .search
            .preview
            .contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "search preview leaked the raw secret: {:?}",
        entry.search.preview,
    );
    assert!(entry.search.preview.contains("[REDACTED]"));
}

#[test]
fn apply_secret_handling_store_full_keeps_body() {
    let raw = "token = ghp_abcdefghijklmnopqrstuvwxyz123456";
    let mut entry = EntryFactory::from_text(raw);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreFull);

    assert_eq!(action, SecretAction::Persist);
    // The raw body is retained (the explicit opt-in)...
    assert_eq!(entry.plain_text(), Some(raw));
    // ...but the preview is still scrubbed so the default DTO path can't
    // leak the raw secret.
    assert!(
        !entry
            .search
            .preview
            .contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "search preview leaked the raw secret under StoreFull: {:?}",
        entry.search.preview,
    );
    assert!(entry.search.preview.contains("[REDACTED]"));
}

#[test]
fn apply_secret_handling_store_redacted_scrubs_alternative_representations() {
    // The capture pipeline persists a snapshot's HTML / RTF / plain
    // alternatives in `pending_representations` verbatim. A redaction that
    // only rewrote the primary body would leave the raw secret in those
    // side reps, and `insert_pending_representations` would persist it —
    // re-creating the leak the primary redaction just closed. The fix
    // makes `apply_secret_handling` self-contained: it drops the
    // alternatives and realigns `representation_set_hash` so a caller that
    // doesn't go through the daemon capture loop still can't leak.
    use crate::{RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation};

    let secret = "token = ghp_abcdefghijklmnopqrstuvwxyz123456";
    let mut entry = EntryFactory::from_text(secret);
    entry.pending_representations = vec![
        StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText(secret.to_owned()),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "text/html".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText(format!("<p>{secret}</p>")),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "text/rtf".to_owned(),
            ordinal: 2,
            data: RepresentationDataRef::InlineText(format!("{{\\rtf1\\ansi {secret}}}")),
        },
    ];

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);

    // No alternative may survive to reach `entry_representations`.
    assert!(
        entry.pending_representations.is_empty(),
        "raw-secret alternatives must be dropped, got: {:?}",
        entry.pending_representations,
    );
    // The set hash must track the redacted primary (so storage falls back
    // to its primary-only insert path), not the stale raw-rep hash.
    assert_eq!(
        entry.metadata.representation_set_hash.as_ref(),
        Some(&entry.metadata.content_hash),
        "representation_set_hash must realign to the redacted primary content hash",
    );
}

#[test]
fn apply_secret_handling_store_redacted_preserves_image_body() {
    // An image classified Secret because of a markup alternative (the
    // factory's image+html shape) must keep its bytes: the secret lives in
    // the HTML rep — dropped by the fall-through — not in the image itself.
    // A prior version redacted `plain_text()` (None → "") and overwrote the
    // body with an empty Text entry, silently destroying the image.
    use crate::{ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot};
    use time::OffsetDateTime;

    let png_bytes = vec![137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13];
    let secret = "token = ghp_abcdefghijklmnopqrstuvwxyz123456";
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("img-secret-html"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![
            ClipboardRepresentation {
                mime_type: "image/png".to_owned(),
                data: ClipboardData::Bytes(png_bytes.clone()),
            },
            ClipboardRepresentation {
                mime_type: "text/html".to_owned(),
                data: ClipboardData::Text(format!("<p>{secret}</p>")),
            },
        ],
    };
    let mut entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
    assert!(matches!(entry.content, ClipboardContent::Image(_)));
    let image_hash = entry.metadata.content_hash.clone();

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(
        entry.sensitivity,
        Sensitivity::Secret,
        "the HTML alternative's secret must drive the verdict",
    );

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);

    // The image body survives untouched...
    match &entry.content {
        ClipboardContent::Image(img) => {
            assert_eq!(
                img.pending_bytes.as_deref(),
                Some(png_bytes.as_slice()),
                "image bytes must be preserved, not replaced with redacted text",
            );
        }
        other => panic!("image body must be preserved, got {other:?}"),
    }
    // ...keyed off the binary payload, not the empty-string hash a redacted
    // text body would have produced.
    assert_eq!(entry.metadata.content_hash, image_hash);
    // The secret-bearing HTML alternative is still dropped and the set hash
    // realigns to the image primary so storage takes the primary-only path.
    assert!(
        entry.pending_representations.is_empty(),
        "raw-secret alternatives must be dropped, got: {:?}",
        entry.pending_representations,
    );
    assert_eq!(
        entry.metadata.representation_set_hash.as_ref(),
        Some(&entry.metadata.content_hash),
    );
}

#[test]
fn apply_secret_handling_store_redacted_redacts_file_list_paths() {
    // A file path can itself be the secret (e.g. one embedding an
    // API-key-shaped token). StoreRedacted must scrub it from the stored
    // paths / display text — neither preserving the raw path (which would
    // leak it into `content_json`) nor collapsing the entry to empty Text.
    use crate::{ClipboardData, ClipboardRepresentation, ClipboardSequence, ClipboardSnapshot};
    use time::OffsetDateTime;

    let token = "ghp_abcdefghijklmnopqrstuvwxyz123456";
    let snapshot = ClipboardSnapshot {
        sequence: ClipboardSequence::content_hash("fl-secret"),
        captured_at: OffsetDateTime::now_utc(),
        source: None,
        representations: vec![ClipboardRepresentation {
            mime_type: "text/uri-list".to_owned(),
            data: ClipboardData::FilePaths(vec![
                format!("/tmp/{token}.txt"),
                "/tmp/safe.txt".to_owned(),
            ]),
        }],
    };
    let mut entry = EntryFactory::from_snapshot(snapshot).expect("entry should build");
    assert!(matches!(entry.content, ClipboardContent::FileList(_)));

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(
        entry.sensitivity,
        Sensitivity::Secret,
        "a token embedded in a path must drive the verdict",
    );

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);

    // Still a FileList (structure preserved), with the secret scrubbed from
    // both the paths and the display text.
    match &entry.content {
        ClipboardContent::FileList(list) => {
            assert!(
                !list.paths.iter().any(|p| p.contains(token)),
                "raw token must not survive in stored paths: {:?}",
                list.paths,
            );
            assert!(
                list.paths.iter().any(|p| p.contains("[REDACTED]")),
                "the secret path must be redacted: {:?}",
                list.paths,
            );
            assert!(
                !list.display_text.contains(token),
                "raw token leaked into display_text: {:?}",
                list.display_text,
            );
            // The hash re-keys off the redacted display text.
            assert_eq!(
                entry.metadata.content_hash,
                ContentHash::sha256(list.display_text.as_bytes()),
            );
        }
        other => panic!("file list structure must be preserved, got {other:?}"),
    }
    // The alternatives are dropped and the set hash realigns to the primary.
    assert!(entry.pending_representations.is_empty());
    assert_eq!(
        entry.metadata.representation_set_hash.as_ref(),
        Some(&entry.metadata.content_hash),
    );
}

#[test]
fn apply_secret_handling_store_full_also_drops_alternatives() {
    // StoreFull keeps the raw *primary* body (the explicit opt-in) but
    // must still drop the duplicate HTML / RTF / plain alternatives: they
    // are extra raw copies the user didn't separately consent to persist,
    // and keeping them widens the at-rest footprint of the secret.
    use crate::{RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation};

    let secret = "token = ghp_abcdefghijklmnopqrstuvwxyz123456";
    let mut entry = EntryFactory::from_text(secret);
    entry.pending_representations = vec![
        StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText(secret.to_owned()),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "text/html".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText(format!("<p>{secret}</p>")),
        },
    ];

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreFull);
    assert_eq!(action, SecretAction::Persist);
    // Raw primary body is retained...
    assert_eq!(entry.plain_text(), Some(secret));
    // ...but the alternatives are dropped and the set hash realigned.
    assert!(entry.pending_representations.is_empty());
    assert_eq!(
        entry.metadata.representation_set_hash.as_ref(),
        Some(&entry.metadata.content_hash),
    );
}

#[test]
fn apply_secret_handling_noop_for_non_secret() {
    // Private/Public entries must not be touched even if the user
    // selected `Block` — the setting only governs Secret-tagged rows.
    let mut entry = EntryFactory::from_text("ordinary clipboard text");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = Sensitivity::Public;

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::Block);
    assert_eq!(action, SecretAction::Persist);
    assert_eq!(entry.plain_text(), Some("ordinary clipboard text"));
}

/// Returns true if `text` contains any contiguous run of ASCII digits
/// whose length is at least `min`. Used by the credit-card redaction
/// tests to guard against a partial scrub that strips formatting but
/// leaves the digits behind — `!contains("4111")` catches obvious leaks
/// but not `41 11 11 11 11 11 11 11`.
fn contains_digit_run_at_least(text: &str, min: usize) -> bool {
    let mut run = 0usize;
    for ch in text.chars() {
        if ch.is_ascii_digit() {
            run += 1;
            if run >= min {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

#[test]
fn redact_text_scrubs_credit_cards() {
    // Regression for the gap where `sensitive_regexes()` had no CC rule
    // and PANs survived even under the default `StoreRedacted` policy.
    // Assert both that the redaction marker appears and that no digit
    // run of 13+ digits (the shortest valid PAN length) remains
    // anywhere in the output — guarding against a partial scrub that
    // strips the spaces but leaves the digits in place.
    for pan in TEST_CREDIT_CARDS {
        let redacted = redact_text(pan);
        assert!(
            redacted.contains("[REDACTED]"),
            "expected redaction marker for {pan:?}, got {redacted:?}",
        );
        assert!(
            !redacted.contains(pan),
            "spaced PAN survived in {redacted:?}",
        );
        let stripped = pan.replace([' ', '-'], "");
        assert!(
            !redacted.contains(stripped.as_str()),
            "contiguous PAN digits leaked from {pan:?}: {redacted:?}",
        );
        assert!(
            !contains_digit_run_at_least(&redacted, 13),
            "digit run ≥13 (PAN-shaped) survived in {redacted:?}",
        );
    }
}

#[test]
fn redact_text_scrubs_private_keys() {
    // PEM blocks land as `[REDACTED]` — the regex matches from BEGIN to
    // either END or end-of-input, so a Secret-classified key never
    // survives the redaction pass.
    let redacted = redact_text(SAMPLE_PRIVATE_KEY);
    assert!(
        !redacted.contains("BEGIN RSA PRIVATE KEY"),
        "private-key body leaked: {redacted:?}",
    );
    assert!(
        redacted.contains("[REDACTED]"),
        "expected redaction marker, got {redacted:?}",
    );
}

#[test]
fn otp_redacted_preview_does_not_leak_raw_code() {
    // OTPs classify as Secret and `redacted_preview` runs through
    // `classifier.redact`, which now strips the bare 6–8 digit body —
    // so the preview is `[REDACTED]` rather than the raw code.
    let entry = EntryFactory::from_text("482915");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
    let preview = result.redacted_preview.expect("Secret must yield preview");
    assert!(
        !preview.contains("482915"),
        "OTP preview leaked the raw code: {preview:?}",
    );
    assert!(preview.contains("[REDACTED]"));
}

#[test]
fn store_redacted_round_trip_credit_card_strips_pan() {
    // `SecretHandling::StoreRedacted` rewrites the body through
    // `classifier.redact`, which now removes Luhn-valid PANs. We assert
    // both forms of the PAN are gone *and* that no PAN-length digit run
    // survives in either the durable body or the normalized search
    // text — a `!body.contains("4111")` style check would miss a
    // partial leak that only kept some digits.
    let pan = "4111 1111 1111 1111";
    let stripped = pan.replace([' ', '-'], "");
    let mut entry = EntryFactory::from_text(pan);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);

    let body = entry.plain_text().expect("persisted body").to_owned();
    assert!(body.contains("[REDACTED]"));
    assert!(!body.contains(pan), "spaced PAN survived: {body:?}");
    assert!(
        !body.contains(stripped.as_str()),
        "contiguous PAN digits survived: {body:?}",
    );
    assert!(
        !contains_digit_run_at_least(&body, 13),
        "PAN-shaped digit run survived in body: {body:?}",
    );
    let norm = &entry.search.normalized_text;
    assert!(
        !norm.contains(stripped.as_str()),
        "contiguous PAN leaked into normalized_text: {norm:?}",
    );
    assert!(
        !contains_digit_run_at_least(norm, 13),
        "PAN-shaped digit run leaked into normalized_text: {norm:?}",
    );
}

#[test]
fn store_redacted_round_trip_private_key_strips_pem() {
    // Mirror of the credit-card case: `StoreRedacted` rewrites the
    // body and the PEM block is gone after the round trip.
    let mut entry = EntryFactory::from_text(SAMPLE_PRIVATE_KEY);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);
    let body = entry.plain_text().expect("persisted body").to_owned();
    assert!(
        !body.contains("BEGIN RSA PRIVATE KEY"),
        "private-key body leaked into stored entry: {body:?}",
    );
    assert!(body.contains("[REDACTED]"));
}

#[test]
fn store_redacted_redacts_api_key_with_user_regex_rules() {
    // Positive coverage: `StoreRedacted` composes the built-in
    // patterns with the user `regex_denylist`, so a clip that pairs a
    // GitHub-style token with a user-configured INTERNAL- prefix gets
    // both scrubbed in the durable body, the content hash, and the
    // search index.
    let raw = "ticket INTERNAL-77 token ghp_abcdefghijklmnopqrstuvwxyz123456";
    let settings = AppSettings {
        regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
        ..AppSettings::default()
    };
    let mut entry = EntryFactory::from_text(raw);
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    // UserRegex would normally Block this entry; force Secret here so
    // we exercise the StoreRedacted path the way the runtime hits it
    // when only the API-key regex fires.
    entry.sensitivity = Sensitivity::Secret;
    let original_hash = entry.metadata.content_hash.value.clone();

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);

    let body = entry.plain_text().expect("persisted body").to_owned();
    assert!(
        !body.contains("ghp_abcdefghijklmnopqrstuvwxyz123456"),
        "GitHub token must not survive in body: {body:?}",
    );
    assert!(
        !body.contains("INTERNAL-77"),
        "user-regex match must not survive in body: {body:?}",
    );
    assert_eq!(
        body.matches("[REDACTED]").count(),
        2,
        "two redactions expected, got {body:?}",
    );
    assert_ne!(
        entry.metadata.content_hash.value, original_hash,
        "content hash must be recomputed for the redacted body",
    );
    assert!(
        !entry.search.normalized_text.contains("ghp_"),
        "search normalized_text leaked GH token: {:?}",
        entry.search.normalized_text,
    );
    assert!(
        !entry.search.normalized_text.contains("internal-77"),
        "search normalized_text leaked user-regex match: {:?}",
        entry.search.normalized_text,
    );
    assert!(
        !entry.search.tokens.iter().any(|tok| tok.contains("ghp_")),
        "tokens index leaked GH token: {:?}",
        entry.search.tokens,
    );
}

#[test]
fn store_redacted_does_not_mutate_private_or_public_entries() {
    // `apply_secret_handling` is a no-op for non-Secret entries — the
    // existing suite covers Public, this pins the Private branch so a
    // password-manager-flagged clip (Private via SourceAppDenylist)
    // can't accidentally fall into the redaction path.
    let mut entry = EntryFactory::from_text("manually-flagged private body");
    entry.sensitivity = Sensitivity::Private;
    let body_before = entry.plain_text().unwrap_or_default().to_owned();
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);
    assert_eq!(
        entry.plain_text().unwrap_or_default(),
        body_before,
        "Private body must not be rewritten by StoreRedacted",
    );
}

#[test]
fn store_redacted_strips_otp_from_persisted_body() {
    // OTPs now flow through `apply_secret_handling`. The default
    // `StoreRedacted` must rewrite the durable body to `[REDACTED]`
    // (with surrounding whitespace preserved) so the raw 6–8 digit
    // code never lands on disk.
    let mut entry = EntryFactory::from_text("482915");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);

    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreRedacted);
    assert_eq!(action, SecretAction::Persist);
    let body = entry.plain_text().expect("redacted body").to_owned();
    assert!(
        !body.contains("482915"),
        "OTP digits leaked into stored entry: {body:?}",
    );
    assert!(body.contains("[REDACTED]"));
}

#[test]
fn store_full_keeps_credit_card_in_body() {
    // `StoreFull` is the explicit "keep raw" opt-in; the body should
    // survive the call intact. The preview half is exercised by the
    // capture-loop tests so it's not duplicated here.
    let pan = "4111 1111 1111 1111";
    let mut entry = EntryFactory::from_text(pan);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    assert_eq!(entry.sensitivity, Sensitivity::Secret);
    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::StoreFull);
    assert_eq!(action, SecretAction::Persist);
    assert_eq!(entry.plain_text(), Some(pan));
}

#[test]
fn block_drops_credit_card_secret_without_mutating_body() {
    let pan = "4111 1111 1111 1111";
    let mut entry = EntryFactory::from_text(pan);
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    entry.sensitivity = classifier.classify(&entry).sensitivity;
    let action = classifier.apply_secret_handling(&mut entry, SecretHandling::Block);
    assert_eq!(action, SecretAction::Drop);
    // Block returns Drop so the caller throws the entry away; body
    // must not be touched on the way out.
    assert_eq!(entry.plain_text(), Some(pan));
}

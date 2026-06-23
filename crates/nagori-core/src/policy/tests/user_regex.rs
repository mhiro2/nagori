use crate::EntryFactory;
use crate::settings::MAX_USER_REGEX_COUNT;

use super::super::*;

#[test]
fn user_regex_overlong_pattern_rejected() {
    // A single overlong pattern is almost certainly an adversarial
    // payload — the realistic redaction rules a human types fit well
    // under the cap. The classifier must reject before the regex
    // crate sees the source so a hostile pattern cannot consume the
    // build budget.
    let long = "a".repeat(MAX_USER_REGEX_LEN + 1);
    let settings = AppSettings {
        regex_denylist: vec![long],
        ..AppSettings::default()
    };
    let err = SensitivityClassifier::try_new(settings).unwrap_err();
    assert!(
        matches!(err, AppError::Policy(ref msg) if msg.contains("byte limit")),
        "expected length-limit Policy error, got {err:?}",
    );
}

#[test]
fn user_regex_compile_error_does_not_echo_pattern_body() {
    // A denylist pattern describes the shape of secrets, so a compile failure
    // must not log the pattern verbatim — neither our own message nor the
    // `regex` crate's caret-annotated `Display` may leak it. Only a short
    // prefix and the byte length are allowed through.
    let err = compile_user_regex("SECRETTOKEN(").unwrap_err();
    let AppError::Policy(msg) = err else {
        panic!("expected Policy error, got {err:?}");
    };
    assert!(
        !msg.contains("SECRETTOKEN"),
        "compile error must not echo the pattern body: {msg}"
    );
    assert!(
        msg.contains("is not a valid regular expression"),
        "compile error should name the failure kind: {msg}"
    );
    assert!(
        msg.contains("12 bytes"),
        "compile error should report the byte length: {msg}"
    );
}

#[test]
fn user_regex_over_count_limit_rejected_by_classifier() {
    // `AppSettings::validate` caps the rule count before persistence, but the
    // classifier must enforce the same ceiling itself: a migrated DB row or a
    // hand-built fixture can reach `try_new` without ever passing validation,
    // and each pattern runs against every capture. The count guard is the last
    // line of defence against an unbounded list defeating the per-pattern DoS
    // limits in aggregate.
    let settings = AppSettings {
        regex_denylist: vec!["a".to_owned(); MAX_USER_REGEX_COUNT + 1],
        ..AppSettings::default()
    };
    let err = SensitivityClassifier::try_new(settings).unwrap_err();
    assert!(
        matches!(err, AppError::Policy(ref msg) if msg.contains("at most")),
        "expected count-limit Policy error, got {err:?}",
    );
}

#[test]
fn user_regex_at_count_limit_builds() {
    // Exactly at the cap must still build — the guard rejects "more than", not
    // "at" the limit.
    let settings = AppSettings {
        regex_denylist: vec!["a".to_owned(); MAX_USER_REGEX_COUNT],
        ..AppSettings::default()
    };
    SensitivityClassifier::try_new(settings).expect("count at the limit builds");
}

#[test]
fn user_regex_deep_nesting_rejected() {
    // Catastrophic-backtracking constructions like `((((a*)*)*)*)*`
    // rely on stacked quantified groups. Capping the parser-visible
    // nesting closes that door without preventing flat alternations
    // a user might want to write.
    let pattern = "(".to_owned()
        + &"(".repeat(MAX_USER_REGEX_NESTING)
        + "a"
        + &")".repeat(MAX_USER_REGEX_NESTING)
        + ")";
    let settings = AppSettings {
        regex_denylist: vec![pattern],
        ..AppSettings::default()
    };
    let err = SensitivityClassifier::try_new(settings).unwrap_err();
    assert!(
        matches!(err, AppError::Policy(ref msg) if msg.contains("nesting depth")),
        "expected nesting-limit Policy error, got {err:?}",
    );
}

#[test]
fn user_regex_escaped_parens_do_not_count_toward_nesting() {
    // `\(` and `\)` are literal characters — they must not trip the
    // nesting guard, otherwise users couldn't write a regex matching
    // bracketed identifiers like `\(INTERNAL-\d+\)`.
    let pattern = r"\(INTERNAL-\d+\)";
    let settings = AppSettings {
        regex_denylist: vec![pattern.to_owned()],
        ..AppSettings::default()
    };
    SensitivityClassifier::try_new(settings).expect("escaped parens are not nesting");
}

#[test]
fn user_regex_compiles_within_budget() {
    // Sanity check that realistic patterns still load — the guard is
    // for adversarial input, not "anything with a quantifier".
    let settings = AppSettings {
        regex_denylist: vec![
            r"INTERNAL-\d+".to_owned(),
            r"(?i)acme[_-]?[a-z0-9]{8,}".to_owned(),
        ],
        ..AppSettings::default()
    };
    SensitivityClassifier::try_new(settings).expect("realistic patterns compile");
}

#[test]
fn user_regex_redacts_realistic_user_patterns() {
    // Locks in the docs claim that the example regexes a privacy-minded
    // user actually writes — internal ticket IDs, vendor URLs, in-house
    // token prefixes — both classify and redact end-to-end. Drift on any
    // of these would mean the help text in the Settings UI overstates
    // what the engine can do.
    let settings = AppSettings {
        regex_denylist: vec![
            r"PROJ-\d{4,6}".to_owned(),
            r"https?://example\.atlassian\.net/browse/[A-Z]+-\d+".to_owned(),
            r"acme_(?:live|test)_[a-z0-9]{16,}".to_owned(),
        ],
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).expect("user-style patterns compile");

    let cases: [(&str, &str); 3] = [
        (
            "see ticket PROJ-12345 for context",
            "see ticket [REDACTED] for context",
        ),
        (
            "linked in https://example.atlassian.net/browse/INFRA-42 yesterday",
            "linked in [REDACTED] yesterday",
        ),
        (
            "key=acme_live_abcdef0123456789 must stay local",
            "key=[REDACTED] must stay local",
        ),
    ];

    for (input, expected_redaction) in cases {
        let entry = EntryFactory::from_text(input);
        let result = classifier.classify(&entry);
        // A `regex_denylist` hit must classify as Blocked so the capture
        // pipeline refuses to persist the row — anything weaker would
        // contradict the help text ("Anything that matches is dropped").
        assert_eq!(
            result.sensitivity,
            Sensitivity::Blocked,
            "expected Blocked for {input:?}, got {:?}",
            result.sensitivity,
        );
        assert!(
            result.reasons.contains(&SensitivityReason::UserRegex),
            "expected UserRegex reason for {input:?}, got {:?}",
            result.reasons,
        );
        // The redact path is the one the AI / preview surfaces use, so
        // the user-visible scrubbed form must match the docs promise.
        assert_eq!(classifier.redact(input), expected_redaction);
    }
}

use crate::EntryFactory;

use super::super::*;

#[test]
fn classifies_github_token_as_secret() {
    let entry = EntryFactory::from_text("token = ghp_abcdefghijklmnopqrstuvwxyz123456");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
}

#[test]
fn classifier_detects_secret_inside_alternative_representation() {
    // Capturing alternative representations widens what reaches storage:
    // a snapshot's HTML / RTF alternatives land in entry_representations
    // alongside the primary.
    // If the classifier only inspected the primary's plain projection,
    // a secret hiding inside the HTML alternative would slip through
    // as Public and persist unredacted. Cover both the detection (must
    // be flagged Secret) and the downstream guarantee that the daemon
    // can then scrub alternatives by checking entry.sensitivity.
    use crate::{RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation};

    let mut entry = EntryFactory::from_text("safe-looking note");
    entry.pending_representations = vec![
        StoredClipboardRepresentation {
            role: RepresentationRole::Primary,
            mime_type: "text/plain".to_owned(),
            ordinal: 0,
            data: RepresentationDataRef::InlineText("safe-looking note".to_owned()),
        },
        StoredClipboardRepresentation {
            role: RepresentationRole::Alternative,
            mime_type: "text/html".to_owned(),
            ordinal: 1,
            data: RepresentationDataRef::InlineText(
                "<p>token = ghp_abcdefghijklmnopqrstuvwxyz123456</p>".to_owned(),
            ),
        },
    ];

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
    assert!(
        result.reasons.contains(&SensitivityReason::ApiKeyPattern),
        "alternative-rep API key must surface in reasons: {:?}",
        result.reasons
    );
}

#[test]
fn classifier_detects_secret_inside_file_path_representation() {
    // A file-URL alternative whose path embeds an API-key-shaped token must
    // be scanned too. The primary FileList is covered via `plain_text`, but an
    // alternative `FilePaths` rep would otherwise reach storage unscanned.
    use crate::{RepresentationDataRef, RepresentationRole, StoredClipboardRepresentation};

    let mut entry = EntryFactory::from_text("safe-looking note");
    entry.pending_representations = vec![StoredClipboardRepresentation {
        role: RepresentationRole::Alternative,
        mime_type: "text/uri-list".to_owned(),
        ordinal: 1,
        data: RepresentationDataRef::FilePaths(vec![
            "/tmp/ghp_abcdefghijklmnopqrstuvwxyz123456".to_owned(),
        ]),
    }];

    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
    assert!(
        result.reasons.contains(&SensitivityReason::ApiKeyPattern),
        "file-path alternative API key must surface in reasons: {:?}",
        result.reasons
    );
}

#[test]
fn oversized_gate_counts_markup_not_just_plain_text() {
    // A RichText clip with a tiny plain primary but a large HTML markup
    // (persisted in content_json) must trip the size gate — the gate sums the
    // distinct text-shaped payloads, not only `plain_text`.
    use crate::{ClipboardContent, RichTextContent, RichTextMarkup};

    let big_markup = format!("<p>{}</p>", "x".repeat(64));
    let content = ClipboardContent::RichText(RichTextContent {
        plain_text: "hi".to_owned(),
        markup: Some(big_markup),
        markup_kind: Some(RichTextMarkup::Html),
    });
    let entry = EntryFactory::from_content(content, None, None);
    let settings = AppSettings {
        max_entry_size_bytes: 32,
        ..Default::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();

    let result = classifier.classify(&entry);

    assert!(
        result.reasons.contains(&SensitivityReason::Oversized),
        "markup beyond the ceiling must trip Oversized: {:?}",
        result.reasons
    );
    assert_eq!(result.sensitivity, Sensitivity::Blocked);
}

#[test]
fn classifies_otp_as_secret() {
    // OTPs are now bucketed with private keys / credit cards so the
    // durable body goes through `apply_secret_handling`. Otherwise an
    // OTP-shaped clip stayed as Private and `apply_secret_handling`
    // skipped it — leaving the raw 6-digit code on disk despite the
    // README's "OTPs are redacted or blocked entirely" promise.
    let entry = EntryFactory::from_text("123456");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
}

#[test]
fn otp_detection_off_leaves_digit_body_public() {
    // With `otp_detection` disabled, the digit-only heuristic is skipped, so
    // a 6–8 digit body classifies on its other signals only — here none, so
    // it stays Public with no reasons.
    let entry = EntryFactory::from_text("123456");
    let settings = AppSettings {
        otp_detection: false,
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Public);
    assert!(
        result.reasons.is_empty(),
        "no reasons expected with OTP detection off: {:?}",
        result.reasons,
    );
}

#[test]
fn classifies_password_manager_source_as_blocked() {
    // The default preset (`password_manager_preset_rules`) carries
    // the canonical 1Password bundle ID, so an entry tagged with
    // `com.agilebits.onepassword7` trips `SourceAppDenylist` and
    // ends up `Blocked`. The broad substring heuristic
    // (`PasswordManagerSource`) was removed when the toggle moved
    // onto the user-controllable preset — toggling it off must
    // actually disable the block.
    let mut entry = EntryFactory::from_text("safe-looking value");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: Some("com.agilebits.onepassword7".to_owned()),
        name: Some("1Password 7".to_owned()),
        executable_path: None,
    });
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();

    let result = classifier.classify(&entry);

    assert_eq!(result.sensitivity, Sensitivity::Blocked);
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist)
    );
    assert!(result.redacted_preview.is_none());
}

#[test]
fn classifies_password_manager_source_as_public_when_preset_cleared() {
    // Regression: the legacy substring heuristic used to block
    // anything whose source contained "1password" / "bitwarden" /
    // "keepass" / "password" even when `app_denylist` was empty.
    // After the toggle migration, clearing the preset must
    // actually let the capture through — otherwise the UI toggle
    // is cosmetic.
    let mut entry = EntryFactory::from_text("safe-looking value");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: Some("com.agilebits.onepassword7".to_owned()),
        name: Some("1Password 7".to_owned()),
        executable_path: None,
    });
    let mut settings = AppSettings::default();
    settings.app_denylist.clear();
    let classifier = SensitivityClassifier::try_new(settings).unwrap();

    let result = classifier.classify(&entry);

    assert_ne!(result.sensitivity, Sensitivity::Blocked);
    assert!(
        !result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist)
    );
}

#[test]
fn classifies_macos_bundle_id_typed_rule_as_blocked() {
    // Typed `SourceApp { MacosBundleId }` rules must match on
    // exact bundle_id (case-insensitive). The matcher must NOT
    // fall back to substring matching against name or
    // executable_path — that is what `Pattern` rules are for.
    use crate::settings::{AppDenyRule, RuleSource, SourceAppIdKind};

    let mut entry = EntryFactory::from_text("safe-looking value");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: Some("com.example.target".to_owned()),
        name: Some("Other Name".to_owned()),
        executable_path: None,
    });
    let settings = AppSettings {
        app_denylist: vec![AppDenyRule::SourceApp {
            kind: SourceAppIdKind::MacosBundleId,
            value: "com.example.target".to_owned(),
            label: Some("Target".to_owned()),
            source: RuleSource::Manual,
        }],
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Blocked);
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist),
        "typed bundle ID rule must fire: {:?}",
        result.reasons,
    );
}

#[test]
fn macos_bundle_id_rule_does_not_match_other_bundle_ids() {
    // A typed rule must not accidentally cover unrelated apps that
    // happen to share a substring. `com.example.target-other`
    // contains the rule's exact value as a prefix, but the typed
    // matcher uses equality (not substring), so the entry stays
    // Public.
    use crate::settings::{AppDenyRule, RuleSource, SourceAppIdKind};

    let mut entry = EntryFactory::from_text("ordinary text");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: Some("com.example.target-other".to_owned()),
        name: None,
        executable_path: None,
    });
    let settings = AppSettings {
        // Drop the default preset so the only rule under test is
        // the one defined here.
        app_denylist: vec![AppDenyRule::SourceApp {
            kind: SourceAppIdKind::MacosBundleId,
            value: "com.example.target".to_owned(),
            label: None,
            source: RuleSource::Manual,
        }],
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let result = classifier.classify(&entry);
    assert!(
        !result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist),
        "typed rule must use equality, not substring: {:?}",
        result.reasons,
    );
}

#[test]
fn classifies_windows_exe_name_typed_rule_as_blocked() {
    // Windows captures bring back the executable path; the typed
    // rule compares the basename (without `.exe`,
    // case-insensitively) so a config that names "1password"
    // fires for `C:\Program Files\1Password\1Password.exe`.
    use crate::settings::{AppDenyRule, RuleSource, SourceAppIdKind};

    let mut entry = EntryFactory::from_text("safe-looking value");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: None,
        name: Some("Vendor App".to_owned()),
        executable_path: Some(r"C:\Program Files\Vendor\VendorApp.exe".to_owned()),
    });
    let settings = AppSettings {
        app_denylist: vec![AppDenyRule::SourceApp {
            kind: SourceAppIdKind::WindowsExeName,
            value: "vendorapp".to_owned(),
            label: Some("VendorApp".to_owned()),
            source: RuleSource::Manual,
        }],
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Blocked);
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist),
        "exe-name rule must fire: {:?}",
        result.reasons,
    );
}

#[test]
fn legacy_string_app_denylist_deserialises_as_pattern_rule() {
    // Settings JSON persisted by an older build stored each
    // denylist entry as a bare string. The custom
    // `deserialize_app_denylist` must read that shape and lift
    // each entry into `AppDenyRule::Pattern`, otherwise upgrading
    // would silently wipe a user's existing rules.
    use crate::settings::AppDenyRule;

    let json = r#"{
        "app_denylist": ["1Password", "Bitwarden"]
    }"#;
    let settings: AppSettings =
        serde_json::from_str(json).expect("legacy Vec<String> deserialises");
    assert_eq!(settings.app_denylist.len(), 2);
    assert_eq!(
        settings.app_denylist[0],
        AppDenyRule::Pattern {
            value: "1Password".to_owned()
        },
    );
    assert_eq!(
        settings.app_denylist[1],
        AppDenyRule::Pattern {
            value: "Bitwarden".to_owned()
        },
    );
}

#[test]
fn missing_app_denylist_field_defaults_to_password_manager_preset() {
    // Settings JSON that omits `app_denylist` entirely (e.g. an
    // older row from before the field existed) must fall back to
    // the bundled password-manager preset rather than an empty Vec
    // — otherwise upgrading from a pre-field build would silently
    // drop the default protections.
    use crate::settings::password_manager_preset_rules;

    let json = "{}";
    let settings: AppSettings =
        serde_json::from_str(json).expect("empty settings JSON deserialises");
    assert_eq!(settings.app_denylist, password_manager_preset_rules());
    assert!(
        !settings.app_denylist.is_empty(),
        "preset must seed at least one rule, otherwise the regression is masked",
    );
}

#[test]
fn pattern_rule_preserves_substring_match_behaviour() {
    // `Pattern` rules keep the original case-insensitive substring
    // semantics so a settings snapshot full of legacy strings (now
    // lifted to `Pattern`) keeps blocking the same apps after the
    // upgrade.
    use crate::settings::AppDenyRule;

    let mut entry = EntryFactory::from_text("safe-looking value");
    entry.metadata.source = Some(crate::SourceApp {
        bundle_id: Some("com.example.somepassword".to_owned()),
        name: Some("SomeApp".to_owned()),
        executable_path: None,
    });
    let settings = AppSettings {
        app_denylist: vec![AppDenyRule::Pattern {
            value: "SomePassword".to_owned(),
        }],
        ..AppSettings::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();
    let result = classifier.classify(&entry);
    assert!(
        result
            .reasons
            .contains(&SensitivityReason::SourceAppDenylist),
        "pattern rule must still match by substring: {:?}",
        result.reasons,
    );
}

#[test]
fn classifies_user_regex_match_as_blocked() {
    // The privacy UI advertises `regex_denylist` as "Captures matching
    // any pattern are dropped", so a UserRegex hit must classify as
    // Blocked (the capture pipeline refuses to persist Blocked rows).
    // Anything weaker — Private/Secret — would let the raw text land
    // in SQLite even when the user explicitly opted out of storage.
    let entry = EntryFactory::from_text("ticket INTERNAL-123 must stay local");
    let settings = AppSettings {
        regex_denylist: vec![r"INTERNAL-\d+".to_owned()],
        ..Default::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();

    let result = classifier.classify(&entry);

    assert_eq!(result.sensitivity, Sensitivity::Blocked);
    assert!(result.reasons.contains(&SensitivityReason::UserRegex));
    // Blocked rows are never persisted, so a redacted preview would be
    // dead weight — and emitting one would imply the entry is browsable.
    assert!(result.redacted_preview.is_none());
}

#[test]
fn oversized_entries_are_blocked() {
    let entry = EntryFactory::from_text("abcdef");
    let settings = AppSettings {
        max_entry_size_bytes: 3,
        ..Default::default()
    };
    let classifier = SensitivityClassifier::try_new(settings).unwrap();

    let result = classifier.classify(&entry);

    assert_eq!(result.sensitivity, Sensitivity::Blocked);
    assert!(result.reasons.contains(&SensitivityReason::Oversized));
}

#[test]
fn classifier_otp_preview_does_not_leak_digits() {
    // OTPs now classify as Secret, so `redacted_preview` flows through
    // `classifier.redact`. The preview must land as `[REDACTED]` rather
    // than echoing the raw 6-digit code.
    let entry = EntryFactory::from_text("482931");
    let classifier = SensitivityClassifier::try_new(AppSettings::default()).unwrap();
    let result = classifier.classify(&entry);
    assert_eq!(result.sensitivity, Sensitivity::Secret);
    let preview = result.redacted_preview.expect("Secret preview present");
    assert!(
        !preview.contains("482931"),
        "OTP digits leaked into preview: {preview:?}",
    );
}

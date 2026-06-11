//! Unit tests for the sensitivity policy, grouped by concern: source/rule
//! classification, secret pattern detection, redaction, and user regex
//! validation. They stay `#[cfg(test)]` submodules because they exercise
//! crate-private helpers such as `redact_text` and `luhn_valid`.

mod classifier;
mod detector;
mod redaction;
mod user_regex;

/// Well-known Luhn-valid test PANs from the major issuers' developer
/// docs. Not real cardholder data, but real enough to exercise the
/// classifier and Luhn check end-to-end.
const TEST_CREDIT_CARDS: &[&str] = &[
    "4111 1111 1111 1111",
    "5555 5555 5555 4444",
    "3782 822463 10005",
    "6011 1111 1111 1117",
    "3530 1113 3330 0000",
];

const SAMPLE_PRIVATE_KEY: &str = concat!(
    "-----BEGIN RSA PRIVATE KEY-----\n",
    "MIIEowIBAAKCAQEAzTestKeyMaterialDoNotUseInProduction\n",
    "-----END RSA PRIVATE KEY-----",
);

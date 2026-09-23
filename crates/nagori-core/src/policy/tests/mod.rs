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
    "2223 0031 2200 3222",
    "3622 720627 1667",
    "6200 0000 0000 0005",
    "6759 6498 2643 8453",
    "4222 2222 22222",
    "3088 0000 0000 0009",
    "1354 123456 78911",
];

const SAMPLE_PRIVATE_KEY: &str = concat!(
    "-----BEGIN RSA PRIVATE KEY-----\n",
    "MIIEowIBAAKCAQEAzTestKeyMaterialDoNotUseInProduction\n",
    "-----END RSA PRIVATE KEY-----",
);

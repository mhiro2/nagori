use std::sync::OnceLock;

use regex::{Regex, RegexBuilder};

use crate::{
    AppError, AppSettings, ClipboardContent, ClipboardEntry, ContentHash, RepresentationDataRef,
    Result, Sensitivity, SensitivityReason, SourceApp, make_preview, normalize_text,
    settings::{AppDenyRule, SecretHandling, SourceAppIdKind},
};

/// Hard upper bound on the source byte length of a single user-provided
/// `regex_denylist` entry.
///
/// Anything longer is almost certainly an adversarial pattern crafted to
/// defeat the compile-time `size_limit` guard or to encode catastrophic
/// backtracking via `(.+)+`-shaped nesting. 256 bytes is roomy enough
/// for the realistic redaction rules a human types ("INTERNAL-\d+", a
/// tagged-token regex, …) while keeping the per-classifier compile
/// budget bounded. The cap is on byte length (`str::len`) rather than
/// `chars().count()`: the user-facing rejection message names "byte
/// limit", and the underlying `RegexBuilder::size_limit` budget is
/// itself byte-denominated.
pub const MAX_USER_REGEX_LEN: usize = 256;

/// Maximum nesting depth for parenthesised groups in a user regex.
///
/// Catastrophic backtracking patterns rely on stacking quantified groups
/// like `(a+)+` or `((a*)*)*`; clamping the parser-visible nesting to
/// three levels rules out the obvious shapes without preventing a user
/// from writing `(?:foo|bar|baz)\d+`.
pub const MAX_USER_REGEX_NESTING: usize = 3;

/// Per-pattern compiled-NFA size limit, in bytes (`RegexBuilder::size_limit`).
///
/// The `regex` crate's default is 10 MiB; we trim to 256 KiB so a
/// maliciously crafted alternation cannot inflate the daemon's working
/// set just by being parsed into the NFA.
const USER_REGEX_SIZE_LIMIT: usize = 256 * 1024;
/// Per-pattern lazy-DFA cache limit, in bytes (`RegexBuilder::dfa_size_limit`).
///
/// The lazy DFA is built incrementally during matching from the NFA
/// above; this cap bounds the working-set blow-up from a long subject
/// string against a wide alternation. 1 MiB sits one order above the
/// NFA cap because the DFA materialises subset-construction states on
/// the fly and a tighter cap would force frequent cache flushes on
/// realistic redaction rules. The `regex` crate's default is 2 MiB.
const USER_REGEX_DFA_SIZE_LIMIT: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SensitivityClassification {
    pub sensitivity: Sensitivity,
    pub reasons: Vec<SensitivityReason>,
    pub redacted_preview: Option<String>,
}

/// Outcome of applying `SecretHandling` to a classified `Secret` entry.
///
/// `Persist` means the entry is now safe to insert (either redacted in place
/// or kept full according to the user setting). `Drop` means the user opted
/// to refuse storage entirely (`SecretHandling::Block`); the caller should
/// audit and skip insertion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecretAction {
    Persist,
    Drop,
}

#[derive(Debug, Clone)]
pub struct SensitivityClassifier {
    settings: AppSettings,
    user_regexes: Vec<Regex>,
}

impl SensitivityClassifier {
    /// Build a classifier from validated settings.
    ///
    /// Fails closed if any `regex_denylist` entry can't be compiled — the
    /// previous behaviour silently dropped invalid patterns, which meant a
    /// DB-corruption or ad-hoc deserialize could leave a user thinking
    /// their secret rules were active when they weren't. `save_settings`
    /// already validates patterns before persisting; this guard catches
    /// any other path where bad data reaches the classifier (e.g. a
    /// migrated DB row that bypassed validation, a future test fixture).
    pub fn try_new(settings: AppSettings) -> Result<Self> {
        let mut user_regexes = Vec::with_capacity(settings.regex_denylist.len());
        for pattern in &settings.regex_denylist {
            let compiled = compile_user_regex(pattern)?;
            user_regexes.push(compiled);
        }
        Ok(Self {
            settings,
            user_regexes,
        })
    }

    pub fn classify(&self, entry: &ClipboardEntry) -> SensitivityClassification {
        let mut reasons = Vec::new();
        let text = entry.plain_text().unwrap_or_default();

        if text.len() > self.settings.max_entry_size_bytes {
            reasons.push(SensitivityReason::Oversized);
        }

        if let Some(source) = &entry.metadata.source {
            let source_text = [
                source.name.as_deref(),
                source.bundle_id.as_deref(),
                source.executable_path.as_deref(),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
            if self
                .settings
                .app_denylist
                .iter()
                .any(|rule| rule_matches_source(rule, source, &source_text))
            {
                reasons.push(SensitivityReason::SourceAppDenylist);
            }
            // The legacy hardcoded substring heuristic ("1password" /
            // "bitwarden" / "keepass" / "password" in `source_text`)
            // used to push `PasswordManagerSource` unconditionally.
            // It now lives on the user-controllable `app_denylist`
            // preset (`password_manager_preset_rules`) instead, so
            // toggling "Block password managers" off in Settings
            // actually disables the block. Without this change the
            // toggle would be cosmetic — apps named "PasswordSafe"
            // etc. would still be blocked by the broad substring
            // even after the user cleared every rule.
        }

        // Scan every text-shaped representation, not just the primary's
        // plain projection. The capture pipeline persists HTML/RTF/plain
        // fallbacks verbatim, so a secret hiding inside a markup
        // alternative would otherwise be classified Public (primary plain
        // is innocuous) and land in `entry_representations` unredacted.
        // The detector union still lets the daemon's Secret-clear pass
        // scrub the alternatives before insert.
        self.scan_text_for_patterns(text, &mut reasons);
        for rep in &entry.pending_representations {
            if let RepresentationDataRef::InlineText(rep_text) = &rep.data {
                if rep_text.as_str() == text {
                    continue;
                }
                self.scan_text_for_patterns(rep_text, &mut reasons);
            }
        }

        let sensitivity = if reasons.iter().any(|reason| {
            matches!(
                reason,
                SensitivityReason::SourceAppDenylist
                    | SensitivityReason::Oversized
                    | SensitivityReason::UserRegex
            )
        }) {
            // UserRegex matches drop the entry entirely — the privacy UI
            // promises "Captures matching any pattern are dropped", so a
            // user who configures `regex_denylist` must never see that
            // text persisted to SQLite (even as a redacted body).
            Sensitivity::Blocked
        } else if reasons.iter().any(|reason| {
            matches!(
                reason,
                SensitivityReason::PrivateKeyPattern
                    | SensitivityReason::ApiKeyPattern
                    | SensitivityReason::CreditCardPattern
                    | SensitivityReason::OneTimePasswordPattern
            )
        }) {
            // OTP joins the Secret bucket (rather than Private) so the
            // durable body goes through `apply_secret_handling` and lands
            // as `[REDACTED]` under the default `StoreRedacted`. Without
            // this, an OTP-shaped clip leaked the raw 6–8 digit code into
            // SQLite even though the preview was scrubbed — a regression
            // the README's "OTPs are redacted or blocked entirely" claim
            // would otherwise overstate.
            Sensitivity::Secret
        } else if !reasons.is_empty() {
            Sensitivity::Private
        } else {
            Sensitivity::Public
        };

        SensitivityClassification {
            sensitivity,
            redacted_preview: matches!(sensitivity, Sensitivity::Private | Sensitivity::Secret)
                .then(|| make_preview(&self.redact(text), 180)),
            reasons,
        }
    }

    /// Run every built-in detector and the compiled user regex set against
    /// `text`, appending any matches to `reasons` without duplicates. Used
    /// by `classify` to fold each representation's content into a single
    /// sensitivity verdict.
    fn scan_text_for_patterns(&self, text: &str, reasons: &mut Vec<SensitivityReason>) {
        let push_once = |reason: SensitivityReason, reasons: &mut Vec<SensitivityReason>| {
            if !reasons.contains(&reason) {
                reasons.push(reason);
            }
        };
        if contains_private_key(text) {
            push_once(SensitivityReason::PrivateKeyPattern, reasons);
        }
        if contains_api_key(text) {
            push_once(SensitivityReason::ApiKeyPattern, reasons);
        }
        if contains_credit_card(text) {
            push_once(SensitivityReason::CreditCardPattern, reasons);
        }
        if is_probable_otp(text) {
            push_once(SensitivityReason::OneTimePasswordPattern, reasons);
        }
        if self.user_regexes.iter().any(|regex| regex.is_match(text)) {
            push_once(SensitivityReason::UserRegex, reasons);
        }
    }

    /// Apply both the built-in secret patterns and the user-configured
    /// `regex_denylist` patterns to `text`. This is the canonical redaction
    /// surface — `redacted_preview` calls into it, and AI/clipboard flows
    /// should prefer it over the bare `redact_text` so user-supplied rules
    /// (e.g. internal ticket prefixes) are honoured everywhere a redacted
    /// copy might leave the trust boundary.
    pub fn redact(&self, text: &str) -> String {
        let mut redacted = redact_text(text);
        for regex in &self.user_regexes {
            redacted = regex.replace_all(&redacted, "[REDACTED]").into_owned();
        }
        redacted
    }

    /// Mutate `entry` so its persisted form matches the user-selected
    /// `SecretHandling` for `Sensitivity::Secret` entries. No-op for
    /// non-Secret classifications. Returns whether the caller should
    /// persist (`Persist`) or drop the entry (`Drop`).
    ///
    /// `StoreRedacted` (the default) rewrites `entry.content`, the
    /// SHA-256 content hash, the search preview, and the search document's
    /// normalized text / tokens to the redacted body so the durable copy on
    /// disk can never leak the raw secret. `StoreFull` keeps the original
    /// body (the explicit opt-in) but still rewrites the search preview to the
    /// redacted form, matching the default-DTO contract where a Secret row's
    /// body is hidden behind `include_text` while its preview is shown.
    /// `Block` returns `Drop` so the caller can audit and skip insertion.
    ///
    /// Rewriting `entry.search.preview` here makes the redaction self-contained
    /// at the core-API boundary: the daemon's capture/runtime paths already
    /// overwrite the preview with `classification.redacted_preview` before
    /// calling this, but a caller that invokes `apply_secret_handling` directly
    /// must not be able to persist a raw-secret preview under either policy.
    ///
    /// For the same reason, both persisting policies drop
    /// `pending_representations` and realign `representation_set_hash` to the
    /// (possibly redacted) primary content hash. The capture pipeline collects
    /// the source's HTML / RTF / plain alternatives there verbatim, so a
    /// `classify` that flagged the entry Secret because of a markup
    /// alternative would otherwise leave the raw secret in a side
    /// representation that `insert_pending_representations` persists — defeating
    /// the redaction the primary body just went through.
    pub fn apply_secret_handling(
        &self,
        entry: &mut ClipboardEntry,
        handling: SecretHandling,
    ) -> SecretAction {
        if !matches!(entry.sensitivity, Sensitivity::Secret) {
            return SecretAction::Persist;
        }
        match handling {
            SecretHandling::Block => return SecretAction::Drop,
            SecretHandling::StoreFull => {
                // Keep the raw body (the user opted in) but scrub the preview
                // so the default DTO path — which shows the preview while
                // gating the body behind `include_text` — never surfaces the
                // raw secret.
                let raw = entry.plain_text().unwrap_or_default().to_owned();
                entry.search.preview = make_preview(&self.redact(&raw), 180);
            }
            SecretHandling::StoreRedacted => {
                let raw = entry.plain_text().unwrap_or_default().to_owned();
                let redacted = self.redact(&raw);
                let redacted_normalized = normalize_text(&redacted);
                // Scrub every text-shaped search surface so the index can never
                // carry the raw secret. For an image these derive from an empty
                // plain projection; recomputing them from the redacted body
                // keeps the standalone API self-contained either way. Match the
                // preview cap used by `classify`'s `redacted_preview` so this
                // yields the same scrubbed preview the daemon path produces.
                entry.search.preview = make_preview(&redacted, 180);
                entry.search.tokens = redacted_normalized
                    .split_whitespace()
                    .map(ToOwned::to_owned)
                    .collect();
                entry.search.normalized_text = redacted_normalized;
                // Rewrite the stored body per content kind so a redaction can
                // never destroy a non-text payload (the old code overwrote
                // every Secret with `from_plain_text(redact(plain_text))`,
                // turning an image — whose `plain_text()` is `None` → `""` —
                // into an empty Text entry).
                match &mut entry.content {
                    // Binary primary: the image bytes can't carry a text
                    // secret, so the only secret lives in a markup alternative
                    // (dropped by the fall-through). Keep the bytes; their hash
                    // already keys off the binary payload (see `EntryFactory`).
                    ClipboardContent::Image(_) => {}
                    // A file path can itself be the secret (e.g. one embedding
                    // an API-key-shaped token), so redact every text field in
                    // place — but keep it a FileList rather than collapsing to
                    // an empty Text entry, and re-key the hash off the redacted
                    // display text (matching how `EntryFactory` hashes a file
                    // list). Alternatives are still dropped by the fall-through.
                    ClipboardContent::FileList(list) => {
                        for path in &mut list.paths {
                            *path = self.redact(path);
                        }
                        list.display_text = self.redact(&list.display_text);
                        entry.metadata.content_hash =
                            ContentHash::sha256(list.display_text.as_bytes());
                    }
                    // Text-shaped primary: the secret is in the body itself, so
                    // rewrite it to the redacted text and re-key the hash so
                    // dedup matches what's actually persisted.
                    _ => {
                        entry.metadata.content_hash = ContentHash::sha256(redacted.as_bytes());
                        entry.content = ClipboardContent::from_plain_text(redacted);
                    }
                }
            }
        }
        // Both StoreFull and StoreRedacted fall through here (Block returned
        // above). The source's HTML / RTF / plain alternatives still hold the
        // raw secret verbatim — drop them and realign the set hash to the
        // (now possibly redacted) primary so storage falls back to its
        // primary-only insert path and no alternative can leak the secret.
        entry.pending_representations.clear();
        entry.metadata.representation_set_hash = Some(entry.metadata.content_hash.clone());
        SecretAction::Persist
    }
}

/// Match a single [`AppDenyRule`] against the observed `SourceApp`.
///
/// `SourceApp` rules compare the typed identifier against the
/// corresponding `SourceApp` field with case-insensitive exact match:
/// drift-free in the common "block this bundle ID" case.
/// `Pattern` rules retain the legacy substring behaviour on the
/// joined `name + bundle_id + executable_path` blob so existing
/// free-text entries keep working without a settings migration.
fn rule_matches_source(rule: &AppDenyRule, source: &SourceApp, source_text_lower: &str) -> bool {
    match rule {
        AppDenyRule::Pattern { value } => {
            let needle = value.trim().to_lowercase();
            !needle.is_empty() && source_text_lower.contains(&needle)
        }
        AppDenyRule::SourceApp { kind, value, .. } => {
            let target = value.trim();
            if target.is_empty() {
                return false;
            }
            match kind {
                SourceAppIdKind::MacosBundleId => source
                    .bundle_id
                    .as_deref()
                    .is_some_and(|bid| bid.eq_ignore_ascii_case(target)),
                SourceAppIdKind::WindowsExeName => source
                    .executable_path
                    .as_deref()
                    .and_then(windows_exe_basename)
                    .is_some_and(|basename| basename.eq_ignore_ascii_case(target)),
                SourceAppIdKind::WindowsExecutablePath => source
                    .executable_path
                    .as_deref()
                    .is_some_and(|path| normalize_exe_path(path) == normalize_exe_path(target)),
                // Linux desktop / Flatpak / X11 WM_CLASS are reserved
                // for a future X11 path; the Wayland adapter currently
                // returns `Ok(None)` for the frontmost app, so these
                // never fire on real hardware. Match against bundle_id
                // (`org.example.App`) or name as a best-effort hook so
                // a forward-looking config does not start out broken.
                SourceAppIdKind::LinuxDesktopId
                | SourceAppIdKind::LinuxFlatpakId
                | SourceAppIdKind::X11WmClass => {
                    let id_match = source
                        .bundle_id
                        .as_deref()
                        .is_some_and(|bid| bid.eq_ignore_ascii_case(target));
                    let name_match = source
                        .name
                        .as_deref()
                        .is_some_and(|name| name.eq_ignore_ascii_case(target));
                    id_match || name_match
                }
            }
        }
    }
}

/// Lowercased basename of a Windows-style executable path, with the
/// `.exe` suffix stripped. Accepts both `\` and `/` separators so a
/// user-pasted path normalises the same regardless of how it was
/// captured.
fn windows_exe_basename(path: &str) -> Option<String> {
    let trimmed = path.trim().trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        return None;
    }
    let basename = trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed);
    let lower = basename.to_lowercase();
    Some(lower.strip_suffix(".exe").unwrap_or(&lower).to_owned())
}

/// Normalise a Windows-style executable path for case-insensitive
/// equality. Collapses separators to `\` and lowercases the result —
/// intentionally minimal because deeper normalisation (`Program Files
/// (x86)` / `%LOCALAPPDATA%` / MSIX) adds complexity without
/// increasing confidence that two paths name the same binary.
fn normalize_exe_path(path: &str) -> String {
    path.trim().replace('/', "\\").to_lowercase()
}

/// Compile a user-provided `regex_denylist` pattern with the DoS-resistant
/// limits applied.
///
/// The default `regex` crate compile is generous (10 MiB DFA, no
/// source-length cap), so a hostile pattern can still gobble RAM or
/// trigger near-pathological match times via `(.+)+`-shaped nesting. We
/// cap the pattern source length, the parenthesis nesting (the lever
/// that catastrophic-backtracking constructions rely on), and the
/// compiled-state size so a misconfigured rule cannot wedge the daemon.
pub fn compile_user_regex(pattern: &str) -> Result<Regex> {
    if pattern.len() > MAX_USER_REGEX_LEN {
        return Err(AppError::Policy(format!(
            "regex_denylist entry exceeds {MAX_USER_REGEX_LEN}-byte limit",
        )));
    }
    let nesting = max_paren_nesting(pattern);
    if nesting > MAX_USER_REGEX_NESTING {
        return Err(AppError::Policy(format!(
            "regex_denylist entry has nesting depth {nesting} (limit {MAX_USER_REGEX_NESTING}); reduce parenthesised groups",
        )));
    }
    RegexBuilder::new(pattern)
        .size_limit(USER_REGEX_SIZE_LIMIT)
        .dfa_size_limit(USER_REGEX_DFA_SIZE_LIMIT)
        .build()
        .map_err(|err| AppError::Policy(format!("invalid regex_denylist entry {pattern:?}: {err}")))
}

/// Count the deepest unescaped parenthesis nesting in `pattern`. We only
/// inspect ASCII bytes — the regex DSL's metacharacters are all 7-bit, so
/// multi-byte UTF-8 inside a character class or literal cannot perturb
/// the count.
fn max_paren_nesting(pattern: &str) -> usize {
    let mut depth: usize = 0;
    let mut max_depth: usize = 0;
    let mut chars = pattern.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => {
                // Skip the escaped character so `\(` / `\)` don't perturb depth.
                let _ = chars.next();
            }
            '(' => {
                depth = depth.saturating_add(1);
                if depth > max_depth {
                    max_depth = depth;
                }
            }
            ')' => {
                depth = depth.saturating_sub(1);
            }
            _ => {}
        }
    }
    max_depth
}

pub fn redact_text(text: &str) -> String {
    // Strip the multi-line PEM block first so the inner base64 body can't
    // collide with the API-key heuristics below (which would otherwise leave
    // half the key visible).
    let mut redacted = redact_private_keys(text);
    redacted = redact_credit_cards(&redacted);
    for regex in sensitive_regexes() {
        redacted = regex.replace_all(&redacted, "[REDACTED]").into_owned();
    }
    // OTP is recognised when the *whole* trimmed body is a 6–8 digit run, so
    // redaction mirrors the classifier: only kicks in when the entry itself
    // is the OTP, never against arbitrary 6–8 digit runs in prose (which
    // would maul timestamps, page counts, etc.).
    if is_probable_otp(&redacted) {
        redacted = redact_full_body(&redacted);
    }
    redacted
}

fn sensitive_regexes() -> &'static [Regex] {
    static SENSITIVE: OnceLock<Vec<Regex>> = OnceLock::new();
    SENSITIVE.get_or_init(|| {
        [
            r"AKIA[0-9A-Z]{16}",
            r"github_pat_[A-Za-z0-9_]{20,}",
            r"gh[pousr]_[A-Za-z0-9_]{20,}",
            r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
            r#"(?i)(api[_-]?key|token|secret|password)\s*[:=]\s*['"]?[^'"\s]{8,}"#,
        ]
        .iter()
        .filter_map(|pattern| Regex::new(pattern).ok())
        .collect()
    })
}

fn redact_private_keys(text: &str) -> String {
    static PRIVATE_KEY: OnceLock<Regex> = OnceLock::new();
    let regex = PRIVATE_KEY.get_or_init(|| {
        // Match a PEM block from `-----BEGIN … PRIVATE KEY-----` through the
        // matching `-----END … PRIVATE KEY-----`. Falls back to end-of-input
        // (`\z`) when the END marker is missing — `contains_private_key`
        // flags the entry as soon as BEGIN appears, so the redactor must
        // not leave a tail through that case.
        Regex::new(
            r"(?s)-----BEGIN[^\r\n-]*PRIVATE KEY-----.*?(?:-----END[^\r\n-]*PRIVATE KEY-----|\z)",
        )
        .expect("private-key regex compiles")
    });
    regex.replace_all(text, "[REDACTED]").into_owned()
}

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

fn redact_credit_cards(text: &str) -> String {
    credit_card_candidate_regex()
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let matched = &caps[0];
            let digits: String = matched.chars().filter(char::is_ascii_digit).collect();
            if (13..=19).contains(&digits.len()) && luhn_valid(&digits) {
                "[REDACTED]".to_owned()
            } else {
                matched.to_owned()
            }
        })
        .into_owned()
}

fn redact_full_body(text: &str) -> String {
    // Preserve the surrounding whitespace so consumers that rely on
    // newline-delimited entries don't see a layout shift after redaction.
    let leading = text.len() - text.trim_start().len();
    let trailing = text.len() - text.trim_end().len();
    let mut out = String::with_capacity(leading + "[REDACTED]".len() + trailing);
    out.push_str(&text[..leading]);
    out.push_str("[REDACTED]");
    out.push_str(&text[text.len() - trailing..]);
    out
}

fn contains_private_key(text: &str) -> bool {
    text.contains("-----BEGIN") && text.contains("PRIVATE KEY-----")
}

fn contains_api_key(text: &str) -> bool {
    sensitive_regexes().iter().any(|regex| regex.is_match(text))
}

fn is_probable_otp(text: &str) -> bool {
    let trimmed = text.trim();
    trimmed.len() >= 6 && trimmed.len() <= 8 && trimmed.chars().all(|ch| ch.is_ascii_digit())
}

fn contains_credit_card(text: &str) -> bool {
    // Detection runs candidate-by-candidate (rather than collapsing every
    // digit in the body) so a clip that pairs a PAN with adjacent expiry /
    // CVV digits still classifies as Secret. Earlier whole-string Luhn made
    // `4111 1111 1111 1111 exp 12/30 cvv 123` come out Public — the raw
    // PAN then bypassed `apply_secret_handling` and landed on disk.
    credit_card_candidate_regex().find_iter(text).any(|m| {
        let digits: String = m.as_str().chars().filter(char::is_ascii_digit).collect();
        (13..=19).contains(&digits.len()) && luhn_valid(&digits)
    })
}

fn luhn_valid(digits: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EntryFactory;

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
    fn redacts_common_secret_patterns() {
        let redacted =
            redact_text("api_key = abcdefghijk and token ghp_abcdefghijklmnopqrstuvwxyz");

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
        let classifier =
            SensitivityClassifier::try_new(settings).expect("user-style patterns compile");

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
}

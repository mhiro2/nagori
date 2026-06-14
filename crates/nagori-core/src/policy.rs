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
        // Force-init the built-in detector set now (classifier construction
        // happens at daemon startup) so a broken built-in pattern fails fast
        // here rather than panicking on the first `classify`. `OnceLock` never
        // poisons, so without this the `expect` in `sensitive_regexes` would
        // re-panic on every classify call instead of once at boot. The
        // patterns are test-covered, so this only ever fires after an edit
        // breaks one.
        sensitive_regexes();
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

        // Size gate spans every distinct text-shaped payload that will be
        // persisted, not just the primary plain projection. A clip with a
        // tiny plain primary but a multi-MB HTML/RTF markup (kept in
        // `content_json`) or a large inline-text alternative would otherwise
        // slip past the ceiling unbounded. Dedup by value so the markup —
        // which the capture path also stores as a representation — is counted
        // once.
        let mut text_payloads: Vec<&str> = vec![text];
        if let ClipboardContent::RichText(rich) = &entry.content
            && let Some(markup) = rich.markup.as_deref()
        {
            text_payloads.push(markup);
        }
        for rep in &entry.pending_representations {
            if let RepresentationDataRef::InlineText(rep_text) = &rep.data {
                text_payloads.push(rep_text);
            }
        }
        text_payloads.sort_unstable();
        text_payloads.dedup();
        let classified_len: usize = text_payloads.iter().map(|s| s.len()).sum();
        if classified_len > self.settings.max_entry_size_bytes {
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
            match &rep.data {
                RepresentationDataRef::InlineText(rep_text) => {
                    if rep_text.as_str() != text {
                        self.scan_text_for_patterns(rep_text, &mut reasons);
                    }
                }
                // A file-URL alternative can itself carry a secret (a path
                // embedding an API-key-shaped token). Scan the joined paths
                // too — the primary FileList is already covered via `text`,
                // but an alternative `FilePaths` rep would otherwise go
                // unscanned and land in `entry_representations` unredacted.
                RepresentationDataRef::FilePaths(paths) => {
                    let joined = paths.join("\n");
                    if joined != text {
                        self.scan_text_for_patterns(&joined, &mut reasons);
                    }
                }
                RepresentationDataRef::DatabaseBlob(_) => {}
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
        .map_err(|err| AppError::Policy(redacted_regex_build_error(pattern, &err)))
}

/// Build a rejection message for a `regex_denylist` pattern that fails to
/// compile, without echoing the pattern body.
///
/// A denylist pattern describes the *shape of the secrets the user wants to
/// keep out of storage*, so it must not be logged or audited verbatim — and
/// the `regex` crate's own error `Display` quotes the offending pattern with a
/// caret, which would leak it just as surely as our own format string did. We
/// surface only a short prefix and the byte length (enough to identify which
/// rule the user must fix in the settings UI, where they can already see their
/// own patterns) plus a coarse reason derived from the error kind.
fn redacted_regex_build_error(pattern: &str, err: &regex::Error) -> String {
    let prefix: String = pattern.chars().take(8).collect();
    let detail = match err {
        regex::Error::CompiledTooBig(limit) => {
            format!("compiles to more than {limit} bytes")
        }
        _ => "is not a valid regular expression".to_owned(),
    };
    format!(
        "regex_denylist entry (prefix {prefix:?}, {} bytes) {detail}",
        pattern.len(),
    )
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
        // These are static, test-covered patterns: a compile failure is a
        // programming error, and silently dropping the broken one (the old
        // `filter_map(.ok())`) would fail open by skipping that detector on
        // a security path. Panic instead, like the other built-in regexes.
        .map(|pattern| Regex::new(pattern).expect("built-in sensitive regex compiles"))
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
mod tests;

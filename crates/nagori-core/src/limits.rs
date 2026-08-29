//! Wire and storage size ceilings shared across crates.
//!
//! The IPC newline-delimited JSON transport refuses lines larger than
//! [`MAX_IPC_BYTES`]. Any clipboard entry that the user can configure to be
//! captured (`max_entry_size_bytes`) has to fit into the same envelope after
//! JSON escaping, otherwise the desktop / CLI surfaces silently fail to load
//! it back. To keep the two consistent we expose the IPC ceiling here and use
//! it as the upper bound for `max_entry_size_bytes`, leaving headroom for the
//! envelope (token, request kind, JSON quoting, etc).

/// Maximum size of a single IPC line (request or response) in bytes. Lines
/// larger than this are rejected by both ends of the transport.
pub const MAX_IPC_BYTES: usize = 1024 * 1024;

/// Upper bound for the user-tunable `max_entry_size_bytes` setting.
///
/// Set to ~75% of [`MAX_IPC_BYTES`] so that even with JSON escaping (worst
/// case 6× expansion for control characters, ~1.1× typical for ASCII text)
/// and the envelope overhead, an entry that storage accepts can still cross
/// the IPC boundary. Values above this would create a class of entries that
/// the daemon stores but neither the desktop nor the CLI can read back.
pub const MAX_ENTRY_SIZE_BYTES: usize = (MAX_IPC_BYTES * 3) / 4;

/// Hard cap on how many entries one "copy selection as one clip" action may
/// join.
///
/// The joined text is a single clipboard entry, so it is already bounded by
/// `max_entry_size_bytes`. This cap bounds the *work* instead: without it a
/// selection of N entries makes the daemon read N full bodies out of storage
/// before discovering that the join cannot fit, so a large multi-selection of
/// maximum-size entries would allocate hundreds of megabytes only to fail. 100
/// is far above any plausible hand-made selection (the palette shows 200
/// results at most) while keeping the pre-failure read work bounded.
pub const MAX_COMBINED_COPY_ENTRIES: usize = 100;

/// Hard cap on decoded image pixel count for clipboard image captures and
/// copy-back.
///
/// `max_entry_size_bytes` only inspects the encoded bytes on the wire, but
/// encoded formats like PNG / JPEG / WebP can advertise huge dimensions in
/// a tiny payload (a few-KB PNG can decode to a 16 GB RGBA buffer). Capping
/// the decoded pixel count is the only defence against that asymmetry, and
/// the limit has to be platform-wide because the same encoded bytes can be
/// pushed through capture, copy-back, or a future preview pipeline.
///
/// 64 megapixels keeps the worst-case RGBA buffer at 256 MB — comfortably
/// above an 8K screenshot (~33 MP) but well below the OOM threshold on a
/// typical workstation. The value is intentionally not user-tunable: the
/// only reason to raise it is to accept payloads that would routinely
/// crash the daemon.
pub const MAX_DECODED_IMAGE_PIXELS: u64 = 64 * 1024 * 1024;

/// Upper bound for the user-tunable `max_image_entry_size_bytes` setting.
///
/// Deliberately *not* tied to [`MAX_IPC_BYTES`] the way [`MAX_ENTRY_SIZE_BYTES`]
/// is: image payloads never cross the IPC line as inline JSON. The desktop
/// streams them through the `nagori-image://` custom scheme straight to the
/// `WebView`, copy-back reads the `SQLite` BLOB in-process, and the image
/// `EntryDto` carries only a `mime`/`byte_count` summary — so the only ceilings
/// that bound an image are decode safety ([`MAX_DECODED_IMAGE_PIXELS`], 64 MP →
/// 256 MB RGBA) and the per-representation storage budget, both enforced
/// independently of this value.
///
/// 64 MiB sits comfortably under those while accepting any screenshot a decode
/// can survive. Raising the user setting toward this ceiling is an
/// expert / high-memory choice — raw TIFF/DIB, the decoded RGBA buffer, and the
/// re-encoded PNG can all coexist for one clip — so the shipped default sits
/// far lower (see `settings::default_max_image_entry_size_bytes`).
pub const MAX_IMAGE_ENTRY_SIZE_BYTES: usize = 64 * 1024 * 1024;

/// Per-content-kind byte budgets applied while reading the clipboard and while
/// trimming an entry's stored representation set.
///
/// Text-shaped payloads (plain / html / rtf / file-URL lists) are gated by
/// `text_bytes`, image payloads by `image_bytes`. Keeping the two separate is
/// what lets a multi-megabyte screenshot be captured under
/// [`MAX_IMAGE_ENTRY_SIZE_BYTES`] while a text clip stays bounded by the
/// IPC-tied [`MAX_ENTRY_SIZE_BYTES`]: the same encoded bytes that a screenshot
/// carries would silently fail to load back if measured against the text
/// budget.
///
/// Adapters apply the matching field to each representation as they probe
/// clipboard sizes; the capture loop re-applies the same split authoritatively
/// in `admit` and `ClipboardEntry::trim_alternatives_to_budget`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadBudget {
    pub text_bytes: usize,
    pub image_bytes: usize,
}

impl ReadBudget {
    #[must_use]
    pub const fn new(text_bytes: usize, image_bytes: usize) -> Self {
        Self {
            text_bytes,
            image_bytes,
        }
    }

    /// Budget that applies to a representation, chosen by whether it is an
    /// image payload.
    #[must_use]
    pub const fn for_kind(self, is_image: bool) -> usize {
        if is_image {
            self.image_bytes
        } else {
            self.text_bytes
        }
    }

    /// The largest a single payload may grow regardless of kind. Adapters that
    /// can only bound one cumulative read at the raw-byte boundary use this and
    /// defer the per-kind decision to the content-aware checks downstream.
    #[must_use]
    pub const fn max(self) -> usize {
        if self.text_bytes > self.image_bytes {
            self.text_bytes
        } else {
            self.image_bytes
        }
    }

    /// Cumulative ceiling for an entry holding both an image and its text
    /// alternatives — the most bytes an in-budget clip could legitimately
    /// total. Saturates so a `usize::MAX`-ish budget cannot wrap.
    #[must_use]
    pub const fn total(self) -> usize {
        self.text_bytes.saturating_add(self.image_bytes)
    }
}

/// Bytes reserved inside an IPC line for everything that is not entry text:
/// the response envelope (`{"Entries":[…]}`), the protocol token, and the
/// per-response scalar fields.
pub const IPC_ENVELOPE_RESERVE_BYTES: usize = 8 * 1024;

/// Longest source-app name an entry DTO carries.
///
/// The OS supplies this string (macOS `localizedName`, a Windows executable
/// name) and nothing upstream bounds it, so the DTO truncates it. Without a
/// cap the row's non-text JSON is unbounded and no fixed overhead allowance
/// could be honest. 128 bytes is far past any real application name.
pub const MAX_DTO_SOURCE_APP_NAME_BYTES: usize = 128;

/// Longest MIME string a representation summary carries, truncated for the
/// same reason as [`MAX_DTO_SOURCE_APP_NAME_BYTES`]: the value originates in a
/// clipboard type declared by another process.
pub const MAX_DTO_MIME_BYTES: usize = 64;

/// Longest code-language tag a search-result DTO carries.
///
/// Canonical ids (`json`, `rust`, …) are a handful of bytes, but the column is
/// written per row and a hand-edited one is not bound by that, so the DTO
/// truncates it like the other OS/row-supplied strings.
pub const MAX_DTO_LANGUAGE_BYTES: usize = 32;

/// Most representation summaries one entry DTO carries.
///
/// Real captures hold a handful (primary plus HTML / RTF / plain / file-list
/// alternatives); the cap keeps a hand-edited row from making the summary list
/// the dominant cost of a response.
pub const MAX_DTO_REPRESENTATION_SUMMARIES: usize = 8;

/// Bytes charged for the fixed-width part of one entry row's JSON.
///
/// The id, the three RFC 3339 timestamps, the flags, the counts, and every
/// field name. All bounded by the schema, unlike the strings above.
pub const IPC_ROW_SCALAR_BYTES: usize = 512;

/// Worst-case wire cost of one entry row's non-text JSON.
///
/// Every variable-length part is bounded — the preview at
/// [`crate::PREVIEW_MAX_CHARS`] characters, the source-app name at
/// [`MAX_DTO_SOURCE_APP_NAME_BYTES`], the summaries at
/// [`MAX_DTO_REPRESENTATION_SUMMARIES`] entries of
/// [`MAX_DTO_MIME_BYTES`] — so this figure is derived from those caps at their
/// most expensive escaping (six bytes per byte) rather than guessed. It is the
/// headroom [`MAX_ENTRY_TEXT_WIRE_BYTES`] reserves so that an admitted entry
/// plus its worst-case metadata still fits one frame.
pub const IPC_ROW_OVERHEAD_BYTES: usize = IPC_ROW_SCALAR_BYTES
    + crate::PREVIEW_MAX_CHARS * 4 * 6
    + MAX_DTO_SOURCE_APP_NAME_BYTES * 6
    + MAX_DTO_LANGUAGE_BYTES * 6
    + MAX_DTO_REPRESENTATION_SUMMARIES * (MAX_DTO_MIME_BYTES * 6 + 64);

/// Longest prefix of `text` that is at most `max_bytes` long and ends on a
/// character boundary. Used to enforce the DTO string caps without splitting a
/// multi-byte character.
#[must_use]
pub fn truncate_on_char_boundary(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Budget for the JSON-escaped entry text one IPC response may carry in
/// total, across every row in it.
pub const MAX_RESPONSE_TEXT_WIRE_BYTES: usize = MAX_IPC_BYTES - IPC_ENVELOPE_RESERVE_BYTES;

/// Ceiling on the JSON-escaped length of a single entry's text.
///
/// [`MAX_ENTRY_SIZE_BYTES`] bounds an entry's *raw* bytes, but the wire
/// carries the JSON-escaped form, and escaping is not length-preserving: a
/// control byte becomes the six-byte `\u0000` sequence, so 200 KB of control
/// characters serialise to over 1 MB and breach [`MAX_IPC_BYTES`]. An entry
/// admitted on its raw length alone could therefore be stored and then be
/// unreadable by every client — an orphan row the daemon holds and neither the
/// desktop nor the CLI can fetch.
///
/// Admission measures [`json_escaped_len`] against this ceiling so that
/// "storage accepted it" implies "a single-entry response fits the frame".
/// Text that escapes to no more than ~1.32x its raw size (all ASCII prose,
/// JSON, source code, CJK) is unaffected; only escape-dense payloads are
/// refused.
pub const MAX_ENTRY_TEXT_WIRE_BYTES: usize = MAX_RESPONSE_TEXT_WIRE_BYTES - IPC_ROW_OVERHEAD_BYTES;

/// Length `text` occupies inside a JSON string, excluding the surrounding
/// quotes — i.e. `serde_json::to_string(text).len() - 2`.
///
/// Computed without allocating so admission can reject an oversized payload
/// before a copy of it exists. Mirrors `serde_json`'s escape table: `"` and
/// `\` and the five shorthand control escapes cost two bytes, every other
/// control byte costs six (`\u00XX`), and all remaining bytes — including
/// every non-ASCII UTF-8 byte — pass through unchanged.
#[must_use]
pub fn json_escaped_len(text: &str) -> usize {
    text.bytes().fold(0_usize, |acc, byte| {
        let cost = match byte {
            b'"' | b'\\' | 0x08 | 0x09 | 0x0a | 0x0c | 0x0d => 2,
            0x00..=0x1f => 6,
            _ => 1,
        };
        acc.saturating_add(cost)
    })
}

/// Whether `text` is small enough, once JSON-escaped, for a response carrying
/// it to fit the IPC frame. See [`MAX_ENTRY_TEXT_WIRE_BYTES`].
#[must_use]
pub fn entry_text_fits_wire(text: &str) -> bool {
    json_escaped_len(text) <= MAX_ENTRY_TEXT_WIRE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic pseudo-random corpus generator: a 64-bit LCG picking from
    /// an alphabet that mixes every class the escape table treats differently
    /// (quote, backslash, shorthand control escapes, `\u00XX` control bytes,
    /// plain ASCII, multi-byte CJK). Fixed seeds keep the "property" test
    /// reproducible without pulling in a property-testing dependency.
    fn corpus(seed: u64, len: usize) -> String {
        const ALPHABET: &[char] = &[
            '"',
            '\\',
            '\n',
            '\t',
            '\r',
            '\u{8}',
            '\u{c}',
            '\u{0}',
            '\u{1}',
            '\u{1f}',
            'a',
            'Z',
            '0',
            ' ',
            '{',
            '}',
            '\u{3042}',
            '\u{6f22}',
            '\u{1f600}',
        ];
        let mut state = seed | 1;
        (0..len)
            .map(|_| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                let index = (state >> 33) as usize % ALPHABET.len();
                ALPHABET[index]
            })
            .collect()
    }

    #[test]
    fn json_escaped_len_matches_serde_json_for_every_escape_class() {
        // The admission gate is only as good as this equality: if the
        // predicted length ever undershoots what `serde_json` actually emits,
        // an entry passes admission and then blows the frame on read-back.
        for seed in 0..64_u64 {
            let sample = corpus(seed, 256);
            let encoded = serde_json::to_string(&sample).expect("string serialises");
            assert_eq!(
                json_escaped_len(&sample),
                encoded.len() - 2,
                "escaped length must match serde_json for {sample:?}"
            );
        }
    }

    #[test]
    fn json_escaped_len_covers_the_control_range_byte_by_byte() {
        for byte in 0..=0x7f_u8 {
            let sample = String::from(char::from(byte));
            let encoded = serde_json::to_string(&sample).expect("string serialises");
            assert_eq!(
                json_escaped_len(&sample),
                encoded.len() - 2,
                "escaped length must match serde_json for byte {byte:#04x}"
            );
        }
    }

    #[test]
    fn a_raw_size_limited_entry_of_ascii_always_fits_the_wire_budget() {
        // ASCII prose never escapes, so the raw ceiling is the binding one and
        // a maximum-size text entry must stay admissible.
        const _: () = {
            assert!(MAX_ENTRY_SIZE_BYTES <= MAX_ENTRY_TEXT_WIRE_BYTES);
            assert!(MAX_ENTRY_TEXT_WIRE_BYTES < MAX_IPC_BYTES);
        };
        let text = "a".repeat(MAX_ENTRY_SIZE_BYTES);
        assert!(entry_text_fits_wire(&text));
    }

    #[test]
    fn control_character_payload_under_the_raw_limit_is_refused_by_the_wire_budget() {
        // The case the raw ceiling alone misses: well under
        // `MAX_ENTRY_SIZE_BYTES` in raw bytes, over `MAX_IPC_BYTES` once
        // escaped, therefore storable but unreadable.
        let text = "\u{1}".repeat(200_000);
        assert!(text.len() < MAX_ENTRY_SIZE_BYTES);
        assert!(json_escaped_len(&text) > MAX_IPC_BYTES);
        assert!(!entry_text_fits_wire(&text));
    }

    #[test]
    fn entry_size_leaves_envelope_headroom_under_ipc_ceiling() {
        // Storage validator must never accept an entry whose raw bytes alone
        // would already breach the IPC line cap, even before envelope JSON.
        // Wrapped in `const { ... }` so the check is enforced at compile time
        // and survives clippy's `assertions_on_constants` lint.
        const _: () = {
            assert!(MAX_ENTRY_SIZE_BYTES < MAX_IPC_BYTES);
            // Reserve at least 64 KiB for envelope (token, request kind, quoting).
            assert!(MAX_IPC_BYTES - MAX_ENTRY_SIZE_BYTES >= 64 * 1024);
        };
    }
}

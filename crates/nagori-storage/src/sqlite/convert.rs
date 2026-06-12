use std::str::FromStr;

use nagori_core::{
    AppError, ClipboardContent, ClipboardEntry, ContentHash, ContentKind, EntryId, EntryLifecycle,
    EntryMetadata, HashAlgorithm, Result, SearchCandidate, SearchDocument, Sensitivity, SourceApp,
};
use nagori_search::normalize_text;
use rusqlite::Row;
use time::{OffsetDateTime, UtcOffset, format_description::well_known::Rfc3339};

pub(crate) fn row_to_entry(row: &Row<'_>) -> rusqlite::Result<ClipboardEntry> {
    let id = EntryId::from_str(&row.get::<_, String>("id")?)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let content_json: String = row.get("content_json")?;
    let content: ClipboardContent = serde_json::from_str(&content_json)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let hash = ContentHash {
        algorithm: HashAlgorithm::Sha256,
        value: row.get("content_hash")?,
    };
    let representation_set_hash = row
        .get::<_, Option<String>>("representation_set_hash")?
        .filter(|value| !value.is_empty())
        .map(|value| ContentHash {
            algorithm: HashAlgorithm::Sha256,
            value,
        });
    let source = {
        let name: Option<String> = row.get("source_app_name")?;
        let bundle_id: Option<String> = row.get("source_bundle_id")?;
        let executable_path: Option<String> = row.get("source_executable_path")?;
        (name.is_some() || bundle_id.is_some() || executable_path.is_some()).then_some(SourceApp {
            name,
            bundle_id,
            executable_path,
        })
    };
    let metadata = EntryMetadata {
        created_at: parse_time(&row.get::<_, String>("created_at")?)?,
        updated_at: parse_time(&row.get::<_, String>("updated_at")?)?,
        last_used_at: parse_opt_time(row.get("last_used_at")?)?,
        use_count: row.get::<_, u32>("use_count")?,
        source,
        content_hash: hash,
        representation_set_hash,
    };
    let search = SearchDocument {
        entry_id: id,
        title: row.get("title").unwrap_or(None),
        preview: row.get("preview").unwrap_or_else(|_| {
            nagori_core::make_preview(content.plain_text().unwrap_or_default(), 180)
        }),
        normalized_text: row
            .get("normalized_text")
            .unwrap_or_else(|_| normalize_text(content.plain_text().unwrap_or_default())),
        tokens: Vec::new(),
        language: row.get("language").unwrap_or(None),
    };
    Ok(ClipboardEntry {
        id,
        content,
        metadata,
        search,
        sensitivity: parse_sensitivity_strict(&row.get::<_, String>("sensitivity")?)?,
        lifecycle: EntryLifecycle {
            pinned: row.get::<_, i64>("pinned")? != 0,
            archived: row.get::<_, i64>("archived")? != 0,
            deleted_at: parse_opt_time(row.get("deleted_at")?)?,
            expires_at: parse_opt_time(row.get("expires_at")?)?,
        },
        // `pending_representations` lives in `entry_representations` after
        // insert and is `#[serde(skip)]` on the model — round-tripping
        // through the DB never repopulates it.
        pending_representations: Vec::new(),
    })
}

/// Search-path projection of a candidate row into the fields ranking reads.
///
/// `candidate_content_json` is a CASE-gated copy of `content_json` the
/// candidate queries emit only for image rows (the result row surfaces pixel
/// dimensions) and for rows missing their `search_documents` join (the
/// preview / normalized-text fallback needs the body) — for ordinary text
/// candidates it is NULL, so the potentially large entry body is never
/// deserialized on the per-keystroke search path.
pub(crate) fn row_to_candidate(row: &Row<'_>) -> rusqlite::Result<SearchCandidate> {
    let entry_id = EntryId::from_str(&row.get::<_, String>("id")?)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))?;
    let content_kind = parse_kind_strict(&row.get::<_, String>("content_kind")?)?;
    let content: Option<ClipboardContent> = row
        .get::<_, Option<String>>("candidate_content_json")?
        .map(|json| {
            serde_json::from_str(&json)
                .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
        })
        .transpose()?;
    let fallback_text = || {
        content
            .as_ref()
            .and_then(ClipboardContent::plain_text)
            .unwrap_or_default()
    };
    let normalized_text = match row.get::<_, Option<String>>("normalized_text")? {
        Some(text) => text,
        None => normalize_text(fallback_text()),
    };
    let preview = match row.get::<_, Option<String>>("preview")? {
        Some(preview) => preview,
        None => nagori_core::make_preview(fallback_text(), 180),
    };
    let (image_width, image_height) = match &content {
        Some(ClipboardContent::Image(image)) => (image.width, image.height),
        _ => (None, None),
    };
    Ok(SearchCandidate {
        entry_id,
        normalized_text,
        preview,
        language: row.get("language")?,
        content_kind,
        created_at: parse_time(&row.get::<_, String>("created_at")?)?,
        use_count: row.get("use_count")?,
        pinned: row.get::<_, i64>("pinned")? != 0,
        sensitivity: parse_sensitivity_strict(&row.get::<_, String>("sensitivity")?)?,
        source_app_name: row.get("source_app_name")?,
        image_width,
        image_height,
    })
}

pub(crate) const fn kind_to_str(kind: ContentKind) -> &'static str {
    match kind {
        ContentKind::Text => "text",
        ContentKind::Url => "url",
        ContentKind::Code => "code",
        ContentKind::Image => "image",
        ContentKind::FileList => "file_list",
        ContentKind::RichText => "rich_text",
        ContentKind::Unknown => "unknown",
    }
}

/// Strict reverse of [`kind_to_str`], mirroring [`parse_sensitivity_strict`]:
/// a label this build doesn't know means the schema drifted ahead of it or the
/// column was tampered with, so surface the error instead of coercing the row
/// into [`ContentKind::Unknown`].
pub(crate) fn parse_kind_strict(value: &str) -> rusqlite::Result<ContentKind> {
    match value {
        "text" => Ok(ContentKind::Text),
        "url" => Ok(ContentKind::Url),
        "code" => Ok(ContentKind::Code),
        "image" => Ok(ContentKind::Image),
        "file_list" => Ok(ContentKind::FileList),
        "rich_text" => Ok(ContentKind::RichText),
        "unknown" => Ok(ContentKind::Unknown),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(AppError::storage(format!(
                "unknown content_kind label in DB row: {other:?}"
            ))),
        )),
    }
}

pub(crate) const fn sensitivity_to_str(sensitivity: Sensitivity) -> &'static str {
    match sensitivity {
        Sensitivity::Unknown => "unknown",
        Sensitivity::Public => "public",
        Sensitivity::Private => "private",
        Sensitivity::Secret => "secret",
        Sensitivity::Blocked => "blocked",
    }
}

/// Strict variant for `row_to_entry`. Refuses to coerce a foreign sensitivity
/// label into `Unknown` — a stray value in the DB column means either the
/// schema has drifted ahead of this build (in which case we should refuse
/// to open instead of misclassifying secret rows as `Unknown`) or the column
/// has been tampered with. Either way, returning an error surfaces the issue
/// instead of silently downgrading the sensitivity guard.
pub(crate) fn parse_sensitivity_strict(value: &str) -> rusqlite::Result<Sensitivity> {
    match value {
        "public" => Ok(Sensitivity::Public),
        "private" => Ok(Sensitivity::Private),
        "secret" => Ok(Sensitivity::Secret),
        "blocked" => Ok(Sensitivity::Blocked),
        "unknown" => Ok(Sensitivity::Unknown),
        other => Err(rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(AppError::storage(format!(
                "unknown sensitivity label in DB row: {other:?}"
            ))),
        )),
    }
}

/// Protection-priority rank for [`Sensitivity`], used by the dedupe path to
/// merge the stored row's sensitivity with a re-captured snapshot's. Higher
/// means more protected. Ordering matters for privacy: an unclassified
/// re-capture of the same bytes (e.g. CLI `add`, which never runs the
/// classifier) must not demote a `Secret` row back to `Unknown` and re-expose
/// it through default listings, previews, and embedding.
pub(crate) const fn sensitivity_rank(sensitivity: Sensitivity) -> u8 {
    match sensitivity {
        Sensitivity::Unknown => 0,
        Sensitivity::Public => 1,
        Sensitivity::Private => 2,
        Sensitivity::Secret => 3,
        Sensitivity::Blocked => 4,
    }
}

pub(crate) fn bool_int(value: bool) -> i64 {
    i64::from(value)
}

/// Render the user's normalized query into an FTS5 MATCH expression.
///
/// Each surviving token is wrapped in `"..."` so FTS5 treats it as a
/// phrase string rather than a bareword that could parse as an operator.
/// We *also* split on the FTS5 metacharacters `(`, `)`, `:`, `*`, and `"`
/// in addition to whitespace: a bareword like `foo:bar` would tokenize
/// fine inside quotes, but a query consisting solely of those chars
/// (e.g. `(` or `:`) previously produced `"("` — a phrase that the
/// tokenizer collapses to zero tokens, raising an FTS5 syntax error at
/// runtime. Stripping them at split time keeps the resulting expression
/// well-formed and removes any path for an unmatched `"` or
/// column-filter `:` to leak through unescaped. Empty fragments are
/// discarded so a query of pure punctuation returns the empty string,
/// which the caller treats as "no FTS candidates".
pub(crate) fn fts_query(query: &str) -> String {
    query
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ':' | '*' | '"'))
        .filter(|part| !part.is_empty())
        .map(|part| format!("\"{part}\""))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Renders a timestamp as a UTC RFC 3339 string for storage.
///
/// The value is pinned to UTC *before* formatting so the column never holds an
/// offset-bearing form (e.g. `+09:00`). Every timestamp comparison in storage
/// — retention sweeps (`created_at < cutoff`), period filters (`created_at >=
/// after`), recency ordering (`ORDER BY created_at DESC`) — is a lexicographic
/// string comparison, and that is only sound when all values share one offset.
/// A mix of `Z` and `+09:00` renderings would sort by wall-clock text rather
/// than instant, so an offset bound from the UI could drop or reorder rows.
/// Normalising to UTC here keeps the column monotonic as text whatever offset
/// the caller's `OffsetDateTime` carries, and matches the form every existing
/// row already uses (all writes have always gone through `now_utc()`), so no
/// data migration is needed.
pub(crate) fn format_time(value: OffsetDateTime) -> Result<String> {
    value
        .to_offset(UtcOffset::UTC)
        .format(&Rfc3339)
        .map_err(|err| AppError::storage_with(err.to_string(), err))
}

pub(crate) fn format_opt_time(value: Option<OffsetDateTime>) -> Result<Option<String>> {
    value.map(format_time).transpose()
}

fn parse_time(value: &str) -> rusqlite::Result<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339)
        .map_err(|err| rusqlite::Error::ToSqlConversionFailure(Box::new(err)))
}

// Callers receive `Option<String>` directly from `row.get`; taking ownership avoids extra rebinding.
#[allow(clippy::needless_pass_by_value)]
fn parse_opt_time(value: Option<String>) -> rusqlite::Result<Option<OffsetDateTime>> {
    value.as_deref().map(parse_time).transpose()
}

pub(crate) fn storage_err(err: rusqlite::Error) -> AppError {
    // Keep the rendered message for display parity, and the typed error in
    // the `#[source]` chain so callers can classify the root cause (e.g.
    // `SQLITE_BUSY` vs schema corruption) without string matching.
    AppError::storage_with(err.to_string(), err)
}

pub(crate) fn json_err(err: serde_json::Error) -> AppError {
    AppError::storage_with(err.to_string(), err)
}

// `PoisonError` owns the guard (not `Send`/`'static`), so only the message
// survives here.
pub(crate) fn lock_err<T>(err: &std::sync::PoisonError<T>) -> AppError {
    AppError::storage(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(unix: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(unix).unwrap()
    }

    fn tokyo(unix: i64) -> OffsetDateTime {
        utc(unix).to_offset(UtcOffset::from_hms(9, 0, 0).unwrap())
    }

    #[test]
    fn format_time_normalises_offset_to_utc() {
        // The same instant carried as a `+09:00` value must serialise to the
        // identical UTC string as the UTC value, with no offset suffix left in.
        let instant = 1_780_000_000;
        assert_eq!(
            format_time(tokyo(instant)).unwrap(),
            format_time(utc(instant)).unwrap()
        );
        assert!(!format_time(tokyo(instant)).unwrap().contains('+'));
    }

    #[test]
    fn format_time_keeps_instants_lexicographically_ordered() {
        // An earlier instant expressed with a positive offset must still sort
        // before a later UTC instant once both are normalised. Compared as the
        // raw offset strings (`...+09:00` vs `...Z`) the order would invert.
        let earlier = tokyo(1_780_000_000);
        let later = utc(1_780_003_600); // one hour later
        assert!(format_time(earlier).unwrap() < format_time(later).unwrap());
    }

    #[test]
    fn format_time_round_trips_through_parse_time() {
        let value = OffsetDateTime::from_unix_timestamp_nanos(1_780_000_000_123_456_789).unwrap();
        let stored = format_time(value).unwrap();
        assert_eq!(parse_time(&stored).unwrap(), value);
    }

    #[test]
    fn storage_err_keeps_typed_cause_in_source_chain() {
        // Root-cause classification (e.g. SQLITE_BUSY vs corruption) walks the
        // `#[source]` chain rather than string-matching the message, so the
        // helper must carry the original `rusqlite::Error` as a typed cause.
        let err = storage_err(rusqlite::Error::QueryReturnedNoRows);
        let source = std::error::Error::source(&err).expect("source must be preserved");
        assert!(source.downcast_ref::<rusqlite::Error>().is_some());
    }

    #[test]
    fn json_err_keeps_typed_cause_in_source_chain() {
        let parse_failure = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
        let err = json_err(parse_failure);
        let source = std::error::Error::source(&err).expect("source must be preserved");
        assert!(source.downcast_ref::<serde_json::Error>().is_some());
    }
}

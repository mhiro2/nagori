use serde::{Deserialize, Serialize};

use super::file_path::{fold_home, is_root_only, keep_trailing_segments, trim_trailing_separators};

/// Number of representative basenames a summary carries for multi-file lists.
const REPRESENTATIVE_LIMIT: usize = 2;

/// Number of trailing directory segments kept in a location label. The choice
/// is width-independent: a long home-relative path collapses to its last two
/// segments (`~/Documents/Acme/Reports` → `Acme/Reports`) so the recall-bearing
/// tail survives, and the renderer ellipsizes from there if even that overflows.
const LOCATION_SEGMENTS: usize = 2;

/// A width-independent summary of a captured file list, built so a result row
/// can lead with filenames instead of a shared absolute prefix.
///
/// Every string here is already semantically shortened (home folded to `~`,
/// directory context trimmed to its trailing segments) but carries no pixel
/// assumptions — the renderer fits it to the available width with plain CSS
/// truncation. The summary never includes a raw absolute path: callers gate it
/// to entries whose paths are safe for default output before building it, and
/// the location label is the only directory text it exposes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    /// Total number of paths in the list, regardless of how many are named.
    pub total: usize,
    /// Basenames of the first [`REPRESENTATIVE_LIMIT`] paths, in clipboard
    /// order. Order is preserved rather than sorted so a row matches what the
    /// user actually copied.
    pub representative_names: Vec<String>,
    /// Trailing-trimmed, home-folded directory shared by every path. Present
    /// only when the whole list lives in one directory; absent when the paths
    /// span several (see [`Self::location_count`]) or carry no directory part.
    pub common_parent_display: Option<String>,
    /// Number of distinct parent directories, present only when the list spans
    /// more than one. Lets a row distinguish same-named files in different
    /// places without listing every per-file parent.
    pub location_count: Option<usize>,
}

/// Build a [`FileSummary`] from a captured list of paths, folding the user's
/// `home` directory to `~` when one is supplied. Returns `None` for an empty
/// list (nothing to summarise).
///
/// `home` is passed in rather than read from the environment so the function
/// stays pure and platform-agnostic — the caller resolves the current home and
/// hands over the verbatim string.
#[must_use]
pub fn build_file_summary(paths: &[String], home: Option<&str>) -> Option<FileSummary> {
    if paths.is_empty() {
        return None;
    }

    let representative_names = paths
        .iter()
        .take(REPRESENTATIVE_LIMIT)
        .map(|path| basename_of(path).to_owned())
        .collect();

    // Distinct *immediate* parents decide whether the list reads as "all in one
    // folder" (show that folder) or "scattered" (show how many places). A bare
    // filename with no separator contributes an empty parent, which collapses
    // to "no location" rather than a spurious one.
    let mut distinct_parents: Vec<&str> = Vec::new();
    for path in paths {
        let parent = parent_of(path);
        if !distinct_parents.contains(&parent) {
            distinct_parents.push(parent);
        }
    }

    let (common_parent_display, location_count) = match distinct_parents.as_slice() {
        [only] => (display_location(only, home), None),
        many => (None, Some(many.len())),
    };

    Some(FileSummary {
        total: paths.len(),
        representative_names,
        common_parent_display,
        location_count,
    })
}

/// Render a parent directory (with its trailing separator) as a compact
/// location label: fold the home prefix to `~`, then keep only the trailing
/// [`LOCATION_SEGMENTS`] path segments. Returns `None` for an empty parent so
/// a bare filename surfaces no location.
fn display_location(parent_with_sep: &str, home: Option<&str>) -> Option<String> {
    if parent_with_sep.is_empty() {
        return None;
    }
    // A lone filesystem root (`/`, `\`, `C:\`) is meaningful as-is and has no
    // trailing segments to trim, so surface it verbatim.
    if is_root_only(parent_with_sep) {
        return Some(parent_with_sep.to_owned());
    }
    let stripped = trim_trailing_separators(parent_with_sep);
    let folded = fold_home(stripped, home);
    Some(keep_trailing_segments(&folded, LOCATION_SEGMENTS).to_owned())
}

/// The basename of `path`: the final segment after the last separator, with any
/// trailing separator run stripped first (`/a/b/` → `b`, `/a/b//` → `b`).
fn basename_of(path: &str) -> &str {
    // A bare filesystem root has no segment below it. Surface the root itself
    // (`/`, `C:\`) rather than the empty string trimming would otherwise leave.
    if is_root_only(path) {
        return path;
    }
    let body = trim_trailing_separators(path);
    match body.rfind(['/', '\\']) {
        Some(idx) => &body[idx + 1..],
        // An all-separator path (e.g. `//`) has no real segment; show it as-is
        // rather than collapsing to an empty name.
        None if body.is_empty() => path,
        None => body,
    }
}

/// The parent directory of `path`, *including* its trailing separator
/// (`/a/b/c` → `/a/`, `/a/b/` → `/a/`). Empty when the path has no separator.
/// A trailing separator run is treated as part of the final segment's name so a
/// directory path keys to its own parent rather than itself — this keeps the
/// distinct-parent grouping order-independent.
fn parent_of(path: &str) -> &str {
    let body = trim_trailing_separators(path);
    match body.rfind(['/', '\\']) {
        Some(idx) => &path[..=idx],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn empty_list_has_no_summary() {
        assert_eq!(build_file_summary(&[], Some("/Users/example")), None);
    }

    #[test]
    fn single_file_leads_with_basename_and_trimmed_home_location() {
        let summary = build_file_summary(
            &paths(&["/Users/example/Documents/Acme/Quarterly Reports/report.pptx"]),
            Some("/Users/example"),
        )
        .expect("summary");
        assert_eq!(summary.total, 1);
        assert_eq!(summary.representative_names, vec!["report.pptx".to_owned()]);
        // Home folds to `~`, then the trailing two segments survive the trim.
        assert_eq!(
            summary.common_parent_display.as_deref(),
            Some("Acme/Quarterly Reports")
        );
        assert_eq!(summary.location_count, None);
    }

    #[test]
    fn file_directly_in_home_folds_to_tilde() {
        let summary = build_file_summary(
            &paths(&["/Users/example/notes.txt"]),
            Some("/Users/example"),
        )
        .expect("summary");
        assert_eq!(summary.common_parent_display.as_deref(), Some("~"));
    }

    #[test]
    fn multiple_files_sharing_a_parent_show_that_parent() {
        let summary = build_file_summary(
            &paths(&[
                "/Users/example/Acme/Reports/a.pptx",
                "/Users/example/Acme/Reports/b.xlsx",
                "/Users/example/Acme/Reports/c.pdf",
            ]),
            Some("/Users/example"),
        )
        .expect("summary");
        assert_eq!(summary.total, 3);
        // Only the first two basenames are named, in clipboard order.
        assert_eq!(
            summary.representative_names,
            vec!["a.pptx".to_owned(), "b.xlsx".to_owned()]
        );
        assert_eq!(
            summary.common_parent_display.as_deref(),
            Some("Acme/Reports")
        );
        assert_eq!(summary.location_count, None);
    }

    #[test]
    fn multiple_files_in_different_parents_report_a_location_count() {
        let summary = build_file_summary(
            &paths(&[
                "/Users/example/Acme/a.txt",
                "/Users/example/Globex/b.txt",
                "/Users/example/Acme/c.txt",
            ]),
            Some("/Users/example"),
        )
        .expect("summary");
        assert_eq!(summary.total, 3);
        assert_eq!(summary.common_parent_display, None);
        // Two distinct parents (`Acme`, `Globex`) despite three files.
        assert_eq!(summary.location_count, Some(2));
    }

    #[test]
    fn windows_paths_split_on_backslash_and_keep_drive_separators() {
        let summary = build_file_summary(
            &paths(&[r"C:\Users\ex\proj\sub\report.docx"]),
            Some(r"C:\Users\ex"),
        )
        .expect("summary");
        assert_eq!(summary.representative_names, vec!["report.docx".to_owned()]);
        // Home folds, then the trailing two backslash segments survive.
        assert_eq!(summary.common_parent_display.as_deref(), Some(r"proj\sub"));
    }

    #[test]
    fn root_only_parent_is_kept_verbatim() {
        let posix =
            build_file_summary(&paths(&["/passwd"]), Some("/Users/example")).expect("posix");
        assert_eq!(posix.common_parent_display.as_deref(), Some("/"));

        let drive = build_file_summary(&paths(&[r"C:\boot.ini"]), None).expect("drive");
        assert_eq!(drive.common_parent_display.as_deref(), Some(r"C:\"));
    }

    #[test]
    fn root_directory_entry_names_the_root_itself() {
        // A file list can contain a directory, including a filesystem root.
        // Trimming would otherwise leave an empty basename for `/`.
        let posix = build_file_summary(&paths(&["/"]), Some("/Users/example")).expect("posix");
        assert_eq!(posix.representative_names, vec!["/".to_owned()]);
        assert_eq!(posix.common_parent_display, None);

        let drive = build_file_summary(&paths(&[r"C:\"]), None).expect("drive");
        assert_eq!(drive.representative_names, vec![r"C:\".to_owned()]);
        assert_eq!(drive.common_parent_display, None);
    }

    #[test]
    fn bare_filename_has_no_location() {
        let summary =
            build_file_summary(&paths(&["report.pdf"]), Some("/Users/example")).expect("summary");
        assert_eq!(summary.representative_names, vec!["report.pdf".to_owned()]);
        assert_eq!(summary.common_parent_display, None);
        assert_eq!(summary.location_count, None);
    }

    #[test]
    fn trailing_directory_separators_resolve_to_the_named_segment() {
        // A directory entry keys to its own parent, and its basename is the
        // final named segment rather than an empty string.
        let summary = build_file_summary(
            &paths(&["/Users/example/Projects/build//"]),
            Some("/Users/example"),
        )
        .expect("summary");
        assert_eq!(summary.representative_names, vec!["build".to_owned()]);
        assert_eq!(summary.common_parent_display.as_deref(), Some("~/Projects"));
    }

    #[test]
    fn missing_home_leaves_absolute_segments() {
        let summary =
            build_file_summary(&paths(&["/opt/data/projects/foo.bin"]), None).expect("summary");
        assert_eq!(summary.representative_names, vec!["foo.bin".to_owned()]);
        assert_eq!(
            summary.common_parent_display.as_deref(),
            Some("data/projects")
        );
    }
}

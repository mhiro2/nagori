//! Width-independent helpers for rendering captured file paths basename-first.
//!
//! These mirror the frontend `apps/desktop/src/app/lib/filePath.ts` rule-for-rule
//! so the palette summary, the preview body, and any future CLI parity surface
//! split paths, hoist a shared parent, and read an extension the same way. Every
//! function here is purely semantic shortening (basename / parent extraction,
//! `~` folding, extension detection); pixel-level fitting (CSS ellipsis, segment
//! dropping) stays on the renderer, which alone knows the available width.
//!
//! Path separators and the extension dot are ASCII, so the byte slicing below
//! always lands on a `char` boundary even for multibyte path segments.

/// Strip a trailing run of `/` or `\` (`"/a/b//"` → `"/a/b"`).
#[must_use]
pub fn trim_trailing_separators(path: &str) -> &str {
    path.trim_end_matches(['/', '\\'])
}

/// Whether `path` is a lone filesystem root with nothing below it.
///
/// Covers a POSIX root (`/`), a bare separator (`\`), the bare UNC introducer
/// (`\\`) that two paths on different servers shrink down to, and a Windows
/// drive root such as `C:\` / `C:/`. Such a prefix is too noisy to hoist into a
/// shared header — every row would still need its own absolute prefix — so
/// callers collapse it away.
#[must_use]
pub fn is_root_only(path: &str) -> bool {
    if path == "/" || path == "\\" || path == "\\\\" {
        return true;
    }
    let bytes = path.as_bytes();
    bytes.len() == 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/')
}

/// Replace a leading `home` prefix with `~`, preserving the separator style.
///
/// A no-op when `home` is absent/empty or `path` lives outside it. `home` is
/// passed in rather than read from the environment so the function stays pure
/// and platform-agnostic — the caller resolves the current home and hands over
/// the verbatim string.
#[must_use]
pub fn fold_home(path: &str, home: Option<&str>) -> String {
    let Some(home) = home else {
        return path.to_owned();
    };
    let home = trim_trailing_separators(home);
    if home.is_empty() {
        return path.to_owned();
    }
    if path == home {
        return "~".to_owned();
    }
    if let Some(rest) = path.strip_prefix(home)
        && rest.starts_with(['/', '\\'])
    {
        return format!("~{rest}");
    }
    path.to_owned()
}

/// Return the suffix of `s` covering its last `n` path segments.
///
/// Preserves the original separators. Falls back to the whole string when it
/// has fewer than `n` separators, so a short location keeps any leading `~` or
/// root marker.
#[must_use]
pub fn keep_trailing_segments(s: &str, n: usize) -> &str {
    if n == 0 {
        return "";
    }
    let bytes = s.as_bytes();
    let mut seen = 0;
    let mut idx = bytes.len();
    while idx > 0 {
        idx -= 1;
        if bytes[idx] == b'/' || bytes[idx] == b'\\' {
            seen += 1;
            if seen == n {
                return &s[idx + 1..];
            }
        }
    }
    s
}

/// A path split into its dimmed parent and emphasised basename.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SplitPath {
    /// Parent directory *including* its trailing separator (`"/a/b/"`), or empty
    /// when the path has no parent segment. Pairing it with `base` reproduces
    /// the visual order `<dim>parent/</dim><strong>basename</strong>`.
    pub dir: String,
    /// The final path segment, with any trailing-separator run stripped.
    pub base: String,
    /// A single representative separator when the path ended in one (so it was a
    /// directory), else empty. The caller re-attaches it to `base` so a folder
    /// reads as `foo/` rather than `foo`. A repeated run (`…/dir//`) collapses to
    /// one, keeping a non-normalised path from yielding an empty basename.
    pub trailing: String,
}

/// Split on the last `/` or `\` so Windows-style file lists also light up the
/// basename emphasis. Mirrors the frontend `splitPath`.
#[must_use]
pub fn split_path(path: &str) -> SplitPath {
    let body = trim_trailing_separators(path);
    // The first stripped char stands in for the (possibly repeated) trailing run.
    let trailing = if body.len() < path.len() {
        path[body.len()..]
            .chars()
            .next()
            .map(|c| c.to_string())
            .unwrap_or_default()
    } else {
        String::new()
    };
    match body.rfind(['/', '\\']) {
        None => SplitPath {
            dir: String::new(),
            base: body.to_owned(),
            trailing,
        },
        Some(idx) => SplitPath {
            dir: body[..=idx].to_owned(),
            base: body[idx + 1..].to_owned(),
            trailing,
        },
    }
}

/// Index just past the last separator delimiting parent-from-basename, or 0.
///
/// A trailing separator run (e.g. `/proj/build/` or a non-normalised
/// `/proj/build//`) is treated as part of the directory's own name rather than
/// as the delimiter, so the parent extracted from either is `/proj/` and the
/// entry can render under that header without becoming an empty row. The whole
/// run is stripped — matching [`split_path`] — so a doubled trailing separator
/// does not pin the parent one segment too deep.
#[must_use]
pub fn dir_end_of(s: &str) -> usize {
    let body = trim_trailing_separators(s);
    match body.rfind(['/', '\\']) {
        Some(idx) => idx + 1,
        None => 0,
    }
}

/// Longest common directory prefix shared by every path, trailing separator
/// included (`"/a/b/"`).
///
/// We compare each entry's *parent-directory candidate* ([`dir_end_of`]-trimmed
/// slice) rather than the raw path so the result is order-independent —
/// otherwise a directory entry appearing later than its sibling file would pin
/// the prefix at the directory itself and collapse that row to empty. Operates
/// on character ranges between separators so we never split inside a path
/// segment. Returns `""` for fewer than two paths or when the prefix shrinks to
/// a lone filesystem root.
#[must_use]
pub fn find_common_parent(paths: &[String]) -> String {
    if paths.len() < 2 {
        return String::new();
    }
    let parents: Vec<&str> = paths.iter().map(|p| &p[..dir_end_of(p)]).collect();
    let mut prefix: &str = parents[0];
    for parent in &parents[1..] {
        if prefix.is_empty() {
            break;
        }
        while !prefix.is_empty() && !parent.starts_with(prefix) {
            // Shrink to the next-shorter directory by dropping the trailing
            // separator (ASCII, so `len - 1` is a char boundary) and re-finding
            // the previous one.
            let trimmed = &prefix[..prefix.len() - 1];
            prefix = &trimmed[..dir_end_of(trimmed)];
        }
    }
    if is_root_only(prefix) {
        return String::new();
    }
    prefix.to_owned()
}

/// Parent directory formatted as a location, with its trailing separator dropped
/// (`/tmp/` → `/tmp`) so it reads as a place.
///
/// A filesystem root stays intact — `/`, `\`, and `C:\` are meaningful as-is and
/// `C:\` must not collapse to the drive-relative `C:`.
#[must_use]
pub fn parent_for_display(dir: &str) -> String {
    if is_root_only(dir) {
        dir.to_owned()
    } else {
        trim_trailing_separators(dir).to_owned()
    }
}

/// The lowercased extension of a path or bare filename, or `None` when there is
/// none worth surfacing.
///
/// Yields `None` for an extensionless name (`Makefile`), a leading-dot file
/// (`.env`), a trailing dot (`report.`), or a dot that lives inside a parent
/// directory (`/some.dir/Makefile`). Accepts a full path or a basename alike — a
/// basename simply has no separator, so the parent-dir guard is a no-op for it.
#[must_use]
pub fn extension_of(path_or_name: &str) -> Option<String> {
    let dot = path_or_name.rfind('.')?;
    // The first basename character — a dot at or before it belongs to a parent
    // directory (`/some.dir/x`) or a leading-dot file (`.env`), not an extension.
    let basename_start = path_or_name.rfind(['/', '\\']).map_or(0, |i| i + 1);
    if dot <= basename_start {
        return None;
    }
    if dot == path_or_name.len() - 1 {
        return None;
    }
    Some(path_or_name[dot + 1..].to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owned(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn is_root_only_recognises_roots_across_platforms() {
        for root in ["/", "\\", "\\\\", "C:\\", "c:/"] {
            assert!(is_root_only(root), "{root} should be root-only");
        }
        for non_root in ["/tmp/", "C:\\Users\\", "\\\\server\\share\\", ""] {
            assert!(!is_root_only(non_root), "{non_root} is below the root");
        }
    }

    #[test]
    fn fold_home_replaces_only_a_segment_boundary_prefix() {
        assert_eq!(fold_home("/Users/me/proj", Some("/Users/me")), "~/proj");
        assert_eq!(fold_home("/Users/me", Some("/Users/me")), "~");
        // A sibling that merely shares a string prefix is left untouched.
        assert_eq!(
            fold_home("/Users/meee/proj", Some("/Users/me")),
            "/Users/meee/proj"
        );
        assert_eq!(fold_home("/opt/data", None), "/opt/data");
    }

    #[test]
    fn split_path_splits_posix_windows_and_bare_names() {
        assert_eq!(
            split_path("/Users/me/proj/a.txt"),
            SplitPath {
                dir: "/Users/me/proj/".to_owned(),
                base: "a.txt".to_owned(),
                trailing: String::new(),
            }
        );
        assert_eq!(
            split_path("C:\\Users\\me\\report.docx"),
            SplitPath {
                dir: "C:\\Users\\me\\".to_owned(),
                base: "report.docx".to_owned(),
                trailing: String::new(),
            }
        );
        assert_eq!(
            split_path("bare.txt"),
            SplitPath {
                dir: String::new(),
                base: "bare.txt".to_owned(),
                trailing: String::new(),
            }
        );
    }

    #[test]
    fn split_path_collapses_a_trailing_separator_run() {
        assert_eq!(
            split_path("/proj/build//"),
            SplitPath {
                dir: "/proj/".to_owned(),
                base: "build".to_owned(),
                trailing: "/".to_owned(),
            }
        );
    }

    #[test]
    fn find_common_parent_hoists_a_shared_directory() {
        assert_eq!(
            find_common_parent(&owned(&["/Users/me/proj/a.txt", "/Users/me/proj/b.txt"])),
            "/Users/me/proj/"
        );
    }

    #[test]
    fn find_common_parent_is_order_independent_with_a_directory_entry() {
        // A directory entry and a file inside it share `/proj/` regardless of
        // which order they arrive in — the directory must not pin the prefix at
        // itself and collapse its own row.
        let forward = find_common_parent(&owned(&["/proj/build/", "/proj/build/file.txt"]));
        let reverse = find_common_parent(&owned(&["/proj/build/file.txt", "/proj/build/"]));
        assert_eq!(forward, "/proj/");
        assert_eq!(reverse, "/proj/");
    }

    #[test]
    fn find_common_parent_collapses_a_lone_root() {
        assert_eq!(find_common_parent(&owned(&["/a.txt", "/b.txt"])), "");
        assert_eq!(find_common_parent(&owned(&["C:\\a.txt", "C:\\b.txt"])), "");
        // Different servers shrink to the bare UNC introducer, also collapsed.
        assert_eq!(
            find_common_parent(&owned(&["\\\\s1\\a\\x", "\\\\s2\\b\\y"])),
            ""
        );
    }

    #[test]
    fn find_common_parent_returns_empty_for_fewer_than_two() {
        assert_eq!(find_common_parent(&owned(&["/Users/me/a.txt"])), "");
        assert_eq!(find_common_parent(&[]), "");
    }

    #[test]
    fn parent_for_display_strips_a_trailing_separator_but_keeps_roots() {
        assert_eq!(parent_for_display("/tmp/"), "/tmp");
        assert_eq!(parent_for_display("/"), "/");
        assert_eq!(parent_for_display("C:\\"), "C:\\");
    }

    #[test]
    fn extension_of_handles_dotfiles_paths_and_multi_dot_names() {
        assert_eq!(extension_of("photo.PNG").as_deref(), Some("png"));
        assert_eq!(extension_of("archive.tar.gz").as_deref(), Some("gz"));
        assert_eq!(extension_of("/some.dir/Makefile"), None);
        assert_eq!(extension_of("Makefile"), None);
        assert_eq!(extension_of(".env"), None);
        assert_eq!(extension_of("report."), None);
        assert_eq!(extension_of("C:\\proj\\photo.png").as_deref(), Some("png"));
    }
}

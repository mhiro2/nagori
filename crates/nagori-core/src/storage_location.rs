//! Best-effort detection of the data directory sitting inside a
//! cloud-sync folder (iCloud Drive, Dropbox, `OneDrive`, …).
//!
//! The clipboard history database is plaintext on disk (see
//! `ARCHITECTURE.md` §19). Full-disk encryption defends a powered-off
//! laptop, but it does **not** stop a cloud-sync client from copying the
//! cleartext data directory up to a third-party server. That is the one
//! at-rest leak a user can trip into by accident — pointing the data
//! directory (via `NAGORI_DB_PATH`, or by keeping their home under a
//! synced folder) at a path the sync client watches.
//!
//! This module recognises the common sync roots and lets the surfaces
//! (`nagori doctor`, the desktop Privacy panel) warn about it. The match
//! itself is a **lexical** path-prefix check. Before comparing, the home
//! directory and the data directory are canonicalized best-effort so a
//! symlinked path component (e.g. macOS resolving `/var` →
//! `/private/var`) does not defeat the prefix match; a canonicalize
//! failure falls back to the path as-given. The trade-off is that a
//! *sync root that is itself a symlink* to another location, a vendor not
//! in the table below, or a sync product mounted at a drive letter / FUSE
//! mount outside the home directory (e.g. Google Drive File Stream, a
//! mapped network drive) is not detected — this is a heuristic hint, not a
//! guarantee.

use std::path::{Path, PathBuf};

/// Cloud-sync vendor a matched root belongs to.
///
/// [`Self::Unknown`] is returned when the data directory sits under a
/// recognised *generic* sync mount (e.g. macOS `~/Library/CloudStorage`)
/// whose specific vendor folder name we do not recognise — the location
/// is still synced, we just cannot name the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloudSyncProvider {
    ICloudDrive,
    Dropbox,
    OneDrive,
    GoogleDrive,
    Box,
    Nextcloud,
    OwnCloud,
    Insync,
    Unknown,
}

impl CloudSyncProvider {
    /// Human-readable vendor name for warning text.
    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::ICloudDrive => "iCloud Drive",
            Self::Dropbox => "Dropbox",
            Self::OneDrive => "OneDrive",
            Self::GoogleDrive => "Google Drive",
            Self::Box => "Box",
            Self::Nextcloud => "Nextcloud",
            Self::OwnCloud => "ownCloud",
            Self::Insync => "Insync",
            Self::Unknown => "a cloud-sync folder",
        }
    }
}

/// A positive detection: which provider, and the sync root the data
/// directory was found under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudSyncMatch {
    pub provider: CloudSyncProvider,
    pub matched_root: PathBuf,
}

impl CloudSyncMatch {
    /// One-line summary for operator-facing surfaces (e.g. `nagori
    /// doctor`): `"Dropbox (/Users/x/Dropbox)"`.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{} ({})",
            self.provider.display_name(),
            self.matched_root.display()
        )
    }
}

/// How a [`SyncRoot`] resolves the provider once a prefix match lands.
#[derive(Debug, Clone, Copy)]
enum ProviderResolution {
    /// The root always belongs to this provider.
    Fixed(CloudSyncProvider),
    /// The root is a generic sync mount point (macOS `CloudStorage`); the
    /// provider is guessed from the first path component beneath it. Only
    /// constructed on macOS — the other platforms have no equivalent
    /// single mount directory, so the variant is unused there.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    FromChild,
}

/// One candidate sync root plus how to name its provider.
#[derive(Debug, Clone)]
struct SyncRoot {
    root: PathBuf,
    resolution: ProviderResolution,
}

/// Returns a match when `data_dir` lives inside a known cloud-sync folder.
///
/// Resolves the user's home directory and platform environment, then runs
/// the lexical prefix check. Returns `None` when the home directory cannot
/// be resolved, so a missing `$HOME` fails open (no false warning) rather
/// than erroring.
#[must_use]
pub fn detect_cloud_sync(data_dir: &Path) -> Option<CloudSyncMatch> {
    let home = canonicalize_lenient(&home_dir_from_env()?);
    let roots = candidate_roots(&home, |key| std::env::var_os(key).map(PathBuf::from));
    match_roots(&canonicalize_lenient(data_dir), &roots)
}

/// Resolve symlinks/`.`/`..` best-effort. A canonicalize failure (the path
/// does not exist yet, or is not accessible) falls back to the input so the
/// caller still gets a usable — if unresolved — path to compare lexically.
fn canonicalize_lenient(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

/// Resolve the home directory from the environment without pulling in the
/// `dirs` crate (which `nagori-core` deliberately avoids). `$HOME` on Unix,
/// `%USERPROFILE%` on Windows.
fn home_dir_from_env() -> Option<PathBuf> {
    let key = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(key)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// A fixed-provider sync root rooted at `path`.
const fn fixed_root(path: PathBuf, provider: CloudSyncProvider) -> SyncRoot {
    SyncRoot {
        root: path,
        resolution: ProviderResolution::Fixed(provider),
    }
}

/// Build the per-platform list of candidate sync roots. `env` looks up an
/// environment variable by name (injected so this stays testable).
fn candidate_roots<F>(home: &Path, env: F) -> Vec<SyncRoot>
where
    F: Fn(&str) -> Option<PathBuf>,
{
    use CloudSyncProvider as P;
    let _ = (&home, &env); // not every cfg branch consumes both
    let mut roots: Vec<SyncRoot> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        // Modern File Provider mounts (Dropbox, OneDrive, Google Drive,
        // Box, …) all live under one directory; the vendor is the child.
        roots.push(SyncRoot {
            root: home.join("Library/CloudStorage"),
            resolution: ProviderResolution::FromChild,
        });
        roots.push(fixed_root(
            home.join("Library/Mobile Documents"),
            P::ICloudDrive,
        ));
        roots.push(fixed_root(home.join("Dropbox"), P::Dropbox));
        roots.push(fixed_root(home.join("OneDrive"), P::OneDrive));
        roots.push(fixed_root(home.join("Google Drive"), P::GoogleDrive));
        roots.push(fixed_root(home.join("Box"), P::Box));
        roots.push(fixed_root(home.join("Box Sync"), P::Box));
        roots.push(fixed_root(home.join("Nextcloud"), P::Nextcloud));
    }

    #[cfg(target_os = "windows")]
    {
        // OneDrive exports its synced root path through the environment;
        // honour every variant before falling back to the default folder.
        // Canonicalize each env value: Business/Commercial roots are named
        // like `OneDrive - Org`, and the env casing / `\\?\` form can differ
        // from the canonicalized `data_dir` we compare against, so a raw push
        // would fail the lexical `starts_with`.
        for key in ["OneDrive", "OneDriveConsumer", "OneDriveCommercial"] {
            if let Some(path) = env(key).filter(|p| !p.as_os_str().is_empty()) {
                roots.push(fixed_root(canonicalize_lenient(&path), P::OneDrive));
            }
        }
        roots.push(fixed_root(home.join("OneDrive"), P::OneDrive));
        roots.push(fixed_root(home.join("Dropbox"), P::Dropbox));
        roots.push(fixed_root(home.join("Google Drive"), P::GoogleDrive));
        roots.push(fixed_root(home.join("Box"), P::Box));
    }

    #[cfg(target_os = "linux")]
    {
        roots.push(fixed_root(home.join("Dropbox"), P::Dropbox));
        roots.push(fixed_root(home.join("Nextcloud"), P::Nextcloud));
        roots.push(fixed_root(home.join("ownCloud"), P::OwnCloud));
        roots.push(fixed_root(home.join("OneDrive"), P::OneDrive));
        roots.push(fixed_root(home.join("Insync"), P::Insync));
        roots.push(fixed_root(home.join("Google Drive"), P::GoogleDrive));
    }

    roots
}

/// Lexical prefix check: returns the first root that `data_dir` sits under.
fn match_roots(data_dir: &Path, roots: &[SyncRoot]) -> Option<CloudSyncMatch> {
    roots.iter().find_map(|candidate| {
        if !data_dir.starts_with(&candidate.root) {
            return None;
        }
        match candidate.resolution {
            ProviderResolution::Fixed(provider) => Some(CloudSyncMatch {
                provider,
                matched_root: candidate.root.clone(),
            }),
            ProviderResolution::FromChild => {
                // `data_dir` is under the generic mount; the next component
                // is the vendor folder. Name the provider from it and point
                // the warning at that concrete mount rather than the parent.
                // If `data_dir` is the mount root itself (no child), still
                // warn — the location is synced even though we can't name a
                // vendor — rather than silently returning no match.
                match data_dir
                    .strip_prefix(&candidate.root)
                    .ok()
                    .and_then(|rest| rest.components().next())
                {
                    Some(child) => {
                        let child = Path::new(child.as_os_str());
                        Some(CloudSyncMatch {
                            provider: provider_from_mount_name(&child.to_string_lossy()),
                            matched_root: candidate.root.join(child),
                        })
                    }
                    None => Some(CloudSyncMatch {
                        provider: CloudSyncProvider::Unknown,
                        matched_root: candidate.root.clone(),
                    }),
                }
            }
        }
    })
}

/// Guess the provider from a macOS `CloudStorage` mount folder name such as
/// `Dropbox-Personal`, `OneDrive-Contoso`, or `GoogleDrive-user@host`.
fn provider_from_mount_name(name: &str) -> CloudSyncProvider {
    let lower = name.to_ascii_lowercase();
    if lower.contains("dropbox") {
        CloudSyncProvider::Dropbox
    } else if lower.contains("onedrive") {
        CloudSyncProvider::OneDrive
    } else if lower.contains("googledrive") || lower.contains("google drive") {
        CloudSyncProvider::GoogleDrive
    } else if lower.contains("box") {
        CloudSyncProvider::Box
    } else if lower.contains("icloud") {
        CloudSyncProvider::ICloudDrive
    } else {
        CloudSyncProvider::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixed(root: PathBuf, provider: CloudSyncProvider) -> SyncRoot {
        SyncRoot {
            root,
            resolution: ProviderResolution::Fixed(provider),
        }
    }

    #[test]
    fn matches_data_dir_directly_under_a_fixed_root() {
        let roots = vec![fixed(
            PathBuf::from("/home/u/Dropbox"),
            CloudSyncProvider::Dropbox,
        )];
        let hit = match_roots(Path::new("/home/u/Dropbox/nagori"), &roots).expect("match");
        assert_eq!(hit.provider, CloudSyncProvider::Dropbox);
        assert_eq!(hit.matched_root, PathBuf::from("/home/u/Dropbox"));
    }

    #[test]
    fn ignores_a_data_dir_outside_every_root() {
        let roots = vec![fixed(
            PathBuf::from("/home/u/Dropbox"),
            CloudSyncProvider::Dropbox,
        )];
        assert!(
            match_roots(
                Path::new("/home/u/Library/Application Support/nagori"),
                &roots
            )
            .is_none()
        );
    }

    #[test]
    fn does_not_match_a_sibling_with_a_shared_name_prefix() {
        // `starts_with` is component-wise, so `/home/u/Dropbox-backup` must
        // not be treated as living under `/home/u/Dropbox`.
        let roots = vec![fixed(
            PathBuf::from("/home/u/Dropbox"),
            CloudSyncProvider::Dropbox,
        )];
        assert!(match_roots(Path::new("/home/u/Dropbox-backup/nagori"), &roots).is_none());
    }

    #[test]
    fn derives_provider_and_mount_from_generic_cloudstorage_child() {
        let roots = vec![SyncRoot {
            root: PathBuf::from("/home/u/Library/CloudStorage"),
            resolution: ProviderResolution::FromChild,
        }];
        let hit = match_roots(
            Path::new("/home/u/Library/CloudStorage/OneDrive-Contoso/nagori"),
            &roots,
        )
        .expect("match");
        assert_eq!(hit.provider, CloudSyncProvider::OneDrive);
        assert_eq!(
            hit.matched_root,
            PathBuf::from("/home/u/Library/CloudStorage/OneDrive-Contoso")
        );
    }

    #[test]
    fn unknown_cloudstorage_vendor_still_warns() {
        let roots = vec![SyncRoot {
            root: PathBuf::from("/home/u/Library/CloudStorage"),
            resolution: ProviderResolution::FromChild,
        }];
        let hit = match_roots(
            Path::new("/home/u/Library/CloudStorage/SomeNewVendor-x/nagori"),
            &roots,
        )
        .expect("match");
        assert_eq!(hit.provider, CloudSyncProvider::Unknown);
        assert_eq!(
            hit.matched_root,
            PathBuf::from("/home/u/Library/CloudStorage/SomeNewVendor-x")
        );
    }

    #[test]
    fn generic_mount_root_itself_still_warns_as_unknown() {
        // The data dir sits exactly at the generic mount with no vendor
        // child — still synced, so warn (as Unknown) rather than miss it.
        let roots = vec![SyncRoot {
            root: PathBuf::from("/home/u/Library/CloudStorage"),
            resolution: ProviderResolution::FromChild,
        }];
        let hit = match_roots(Path::new("/home/u/Library/CloudStorage"), &roots).expect("match");
        assert_eq!(hit.provider, CloudSyncProvider::Unknown);
        assert_eq!(
            hit.matched_root,
            PathBuf::from("/home/u/Library/CloudStorage")
        );
    }

    #[test]
    fn candidate_roots_include_a_known_vendor_for_the_host_platform() {
        // Smoke test: the host platform must contribute at least one root,
        // and pointing a data dir under it must match.
        let home = PathBuf::from("/home/tester");
        let roots = candidate_roots(&home, |_| None);
        assert!(!roots.is_empty(), "host platform contributes sync roots");
        let dropbox = home.join("Dropbox/nagori");
        let hit = match_roots(&dropbox, &roots).expect("Dropbox is a candidate on every OS");
        assert_eq!(hit.provider, CloudSyncProvider::Dropbox);
    }

    #[test]
    fn first_matching_root_wins() {
        let roots = vec![
            fixed(PathBuf::from("/sync/a"), CloudSyncProvider::Dropbox),
            fixed(PathBuf::from("/sync/a/b"), CloudSyncProvider::Box),
        ];
        let hit = match_roots(Path::new("/sync/a/b/nagori"), &roots).expect("match");
        assert_eq!(hit.provider, CloudSyncProvider::Dropbox);
    }
}

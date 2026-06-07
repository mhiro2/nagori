// Builds the `nagori-image://` custom-scheme URL the webview fetches for an
// entry's image bytes. The Rust handler (src-tauri/src/image_scheme.rs)
// enforces sensitivity gating and signature validation on every request.
//
// macOS / iOS / Linux use `scheme://localhost/<path>`; Windows / Android use
// `http://<scheme>.localhost/<path>` so the webview's Origin matches the
// fetched URL (otherwise a SecurityError). `useThumb` requests the daemon's
// cached 512px thumbnail (`/thumb/<id>`); otherwise it streams the
// full-resolution original payload (`/<id>`).
//
// The handler ignores the query string (`parse_image_entry_id` reads only the
// path), so the cache-busting `?v=` suffix is a free no-op on the first
// attempt and only forces the webview to actually re-fetch on a retry (the
// response is `Cache-Control: no-store`, but a unique URL guarantees the
// re-fetch).
export function buildImageUrl(id: string, useThumb: boolean, attempt: number): string {
  const isWinAndroid =
    typeof navigator !== 'undefined' && /Windows|Android/i.test(navigator.userAgent);
  const origin = isWinAndroid ? 'http://nagori-image.localhost' : 'nagori-image://localhost';
  const segment = useThumb ? `thumb/${id}` : id;
  const suffix = attempt > 0 ? `?v=${attempt}` : '';
  return `${origin}/${segment}${suffix}`;
}

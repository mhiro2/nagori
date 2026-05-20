//! GUI fallback window shown when the main `AppState` cannot initialise
//! (Linux compositor without `wl_data_control`, corrupted DB, denied
//! data directory, etc.). The fallback surfaces the same wording the
//! CLI's `nagori doctor` / `annotate_linux_clipboard_error` already
//! print so the user does not have to drop to a terminal to learn what
//! happened — see `docs/platforms.md` for the underlying matrix.
//!
//! The window is rendered from an inline `data:text/html;base64,...`
//! URL so it works without bundling a separate asset and stays
//! reachable even if the main frontend dist is missing. The error
//! message is HTML-escaped before embedding so an attacker-controlled
//! error string (e.g. a crafted DB path) cannot inject markup.

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tauri::{AppHandle, Url, WebviewUrl, WebviewWindowBuilder};

/// Tauri window label assigned to the fallback window. Matched in
/// `on_run_event` so closing it tears the process down even though the
/// main window's `CloseRequested` is intercepted and converted into a
/// hide.
pub(crate) const FALLBACK_WINDOW_LABEL: &str = "fallback";

/// URL surfaced in the fallback body so the user can read the platform
/// requirements and troubleshooting steps without a terminal. Points at
/// the canonical `docs/platforms.md` on the default branch — the same
/// document the daemon refers operators to.
const DOCS_URL: &str = "https://github.com/mhiro2/nagori/blob/main/docs/platforms.md";

/// Build the HTML body shown inside the fallback window. The error
/// message is HTML-escaped so a path / OS string containing `<`, `&`
/// etc. cannot escape the `<pre>` block and inject markup. The output
/// is intentionally static (no `<script>`, no inline event handlers,
/// no `<a href>`) so the page works under a strict CSP and so the
/// fallback webview — which has no IPC capability and no managed
/// `AppState` — can never be coerced into navigating to an
/// attacker-controlled URL. The docs URL is rendered as plain text;
/// the user copies it into their browser, which keeps the fallback
/// strictly read-only.
pub(crate) fn build_fallback_html(error_message: &str) -> String {
    let escaped = escape_html(error_message.trim());
    format!(
        "<!DOCTYPE html>\n\
         <html lang=\"en\">\n\
         <head>\n\
         <meta charset=\"utf-8\">\n\
         <title>Nagori — Startup error</title>\n\
         <style>\n\
           :root {{ color-scheme: light dark; }}\n\
           body {{ font-family: -apple-system, system-ui, \"Segoe UI\", Roboto, sans-serif; margin: 0; padding: 24px; line-height: 1.5; }}\n\
           h1 {{ font-size: 18px; margin: 0 0 12px; }}\n\
           pre {{ background: rgba(127, 127, 127, 0.12); padding: 12px; border-radius: 6px; font-size: 13px; white-space: pre-wrap; word-break: break-word; }}\n\
           .footer {{ margin-top: 16px; font-size: 13px; opacity: 0.85; }}\n\
           code {{ font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }}\n\
         </style>\n\
         </head>\n\
         <body>\n\
         <h1>Nagori failed to start</h1>\n\
         <pre>{escaped}</pre>\n\
         <p class=\"footer\">Platform requirements and troubleshooting: <code>{DOCS_URL}</code>. Run <code>nagori doctor</code> in a terminal for diagnostic details.</p>\n\
         </body>\n\
         </html>"
    )
}

/// Wrap the fallback HTML in a `data:` URL the webview can load
/// directly. Base64 keeps the body opaque to URL parsers and avoids
/// percent-encoding edge cases (the `url` crate is otherwise happy to
/// silently re-normalise the data segment).
pub(crate) fn build_fallback_data_url(error_message: &str) -> String {
    let html = build_fallback_html(error_message);
    format!(
        "data:text/html;charset=utf-8;base64,{}",
        STANDARD.encode(html.as_bytes())
    )
}

/// Create the fallback window and show the user the recorded error.
/// The caller is expected to skip the rest of `setup` (background
/// tasks, tray, settings subscribers) because `AppState` is not managed
/// in this branch — closing the window exits the process via
/// `on_run_event`.
pub(crate) fn show_startup_fallback_window(
    app: &AppHandle,
    error_message: &str,
) -> tauri::Result<()> {
    let raw_url = build_fallback_data_url(error_message);
    // `build_fallback_data_url` always returns `data:text/html;…;base64,<base64>`
    // which is a well-formed RFC 2397 URL — the `url` crate parses it as a
    // non-special-scheme URL with an opaque path. A failure here means we
    // emitted something other than that template (programmer error), so a
    // panic-on-expect is the right signal and is exercised by the unit
    // test below.
    let url = Url::parse(&raw_url).expect("fallback data URL must parse");
    WebviewWindowBuilder::new(app, FALLBACK_WINDOW_LABEL, WebviewUrl::External(url))
        .title("Nagori — Startup error")
        .inner_size(560.0, 380.0)
        .min_inner_size(420.0, 280.0)
        .resizable(true)
        .visible(true)
        .center()
        .focused(true)
        .skip_taskbar(false)
        .build()?;
    Ok(())
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error message must be HTML-escaped: an OS string carrying
    /// `<`, `>` or `&` (a crafted DB path, a Wayland adapter error
    /// containing markup-like glyphs) must not escape the `<pre>` block
    /// and inject DOM into the fallback page.
    #[test]
    fn build_fallback_html_escapes_markup_in_error() {
        let html = build_fallback_html("<script>alert('x')</script> & <b>");
        assert!(
            !html.contains("<script>"),
            "raw <script> must not survive escaping: {html}"
        );
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("&amp; &lt;b&gt;"));
    }

    /// The body must surface the canonical docs link so the user can
    /// reach the platform requirements without going back to a
    /// terminal. The link target is asserted verbatim so a typo or a
    /// renamed doc gets caught here rather than during user testing.
    #[test]
    fn build_fallback_html_links_to_platforms_doc() {
        let html = build_fallback_html("anything");
        assert!(
            html.contains(DOCS_URL),
            "docs link must appear in the fallback body"
        );
        assert!(html.contains("nagori doctor"));
    }

    /// `data:` URLs with base64 payloads must parse as URLs so
    /// `WebviewUrl::External` accepts them without a runtime panic.
    /// Base64 over arbitrary HTML can include `+`, `/`, `=` — all of
    /// which are valid in the opaque path of a non-special URL — so the
    /// parse here doubles as a regression test against switching to a
    /// non-tolerant base64 alphabet.
    #[test]
    fn build_fallback_data_url_parses_as_url() {
        let raw = build_fallback_data_url("DB path /tmp/nagori.sqlite is corrupt");
        assert!(raw.starts_with("data:text/html;charset=utf-8;base64,"));
        Url::parse(&raw).expect("fallback data URL must parse");
    }

    /// Trimming the error message keeps stray leading/trailing newlines
    /// (common when the source is an annotated `AppError::Storage`
    /// emitted by `annotate_startup_error`) out of the rendered `<pre>`
    /// so the fallback body does not start with a blank line.
    #[test]
    fn build_fallback_html_trims_whitespace() {
        let html = build_fallback_html("\n  could not open db\n");
        assert!(html.contains("<pre>could not open db</pre>"));
    }

    /// The fallback page must remain a strictly read-only error
    /// surface: no `<a href>` and no `<script>` so the unconfigured
    /// webview (no IPC, no managed `AppState`) cannot be steered into
    /// navigating to or executing attacker-controlled content. The
    /// docs URL still appears verbatim as plain text — the user
    /// copies it into their own browser.
    #[test]
    fn build_fallback_html_has_no_anchor_or_script() {
        let html = build_fallback_html("any error");
        assert!(
            !html.contains("<a "),
            "fallback HTML must not include anchor tags: {html}"
        );
        assert!(
            !html.contains("<script"),
            "fallback HTML must not include script tags: {html}"
        );
    }

    /// Pin the data-URL framing end-to-end: base64-decoding the payload
    /// recovers the exact HTML body that `build_fallback_html`
    /// produced. Without this, a future change to the encoding (e.g.
    /// switching to a URL-safe alphabet that webview decoders reject)
    /// would land without a test failure — `Url::parse` succeeds for
    /// almost any data URL shape, so it cannot catch encoding drift on
    /// its own.
    #[test]
    fn build_fallback_data_url_round_trips_html_body() {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;

        let raw_message = "DB at /tmp/n.sqlite is corrupt";
        let url = build_fallback_data_url(raw_message);
        let body = url
            .strip_prefix("data:text/html;charset=utf-8;base64,")
            .expect("data URL must use the expected mime/encoding prefix");
        let decoded = STANDARD.decode(body).expect("base64 payload decodes");
        let html = String::from_utf8(decoded).expect("payload is UTF-8");
        assert_eq!(html, build_fallback_html(raw_message));
    }
}

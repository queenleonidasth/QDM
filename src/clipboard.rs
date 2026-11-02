//! Opt-in clipboard link detection for QDM.
//!
//! The decision logic here is intentionally free of Win32 calls so the format,
//! debounce and de-duplication rules can be unit-tested on any platform. The UI
//! layer owns the OS clipboard sequence-number poll and text read; this module
//! only decides whether a *change* should surface a non-blocking suggestion.
//!
//! Privacy rules:
//! - We never forward clipboard contents anywhere; only a URL the user explicitly
//!   accepts becomes a download, and even then only after an interaction.
//! - We surface a suggestion *only* when the clipboard is a single well-formed
//!   absolute HTTP(S) URL. Multi-line or free-form text (which may contain private
//!   data) is ignored entirely.

use std::collections::HashSet;

/// Upper bound on a clipboard string we are willing to inspect for a link.
const MAX_TEXT_CHARS: usize = 4096;

/// Extract a single, well-formed absolute HTTP(S) URL from clipboard text.
///
/// Returns `None` unless the trimmed content is exactly one token that parses as an
/// `http`/`https` URL with a host. This is deliberately strict so that arbitrary
/// copied prose, file paths, or multiple lines never trigger a download suggestion.
pub fn extract_url(text: &str) -> Option<String> {
    let trimmed = text.trim().trim_matches('"').trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_TEXT_CHARS {
        return None;
    }
    // A lone URL token: reject anything with internal whitespace (prose, lists,
    // "Download: <url>" captions, or multi-line selections).
    if trimmed.split_whitespace().count() != 1 {
        return None;
    }
    // Reject obvious non-links quickly (no scheme separator at all).
    if !trimmed.contains("://") {
        return None;
    }
    let parsed = url::Url::parse(trimmed).ok()?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    // A host is required; scheme-only or file-like strings are not download links.
    if parsed.host_str().unwrap_or("").is_empty() {
        return None;
    }
    Some(trimmed.to_owned())
}

/// A short, human-friendly label for a suggested link: the last path segment when
/// present, otherwise the host. Used only for display in the suggestion banner.
pub fn display_name(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(parsed) => {
            let last = parsed
                .path_segments()
                .map(|mut s| s.by_ref().rev().find(|seg| !seg.is_empty()))
                .flatten()
                .map(str::to_owned)
                .unwrap_or_default();
            if !last.is_empty() {
                last
            } else {
                parsed.host_str().unwrap_or("link").to_owned()
            }
        }
        Err(_) => url.to_owned(),
    }
}

#[derive(Default)]
pub struct ClipboardWatcher {
    /// Suppress the first change observed after startup/enable so QDM never
    /// re-suggests whatever was already on the clipboard before it looked.
    primed: bool,
    last_shown: Option<String>,
    dismissed: HashSet<String>,
}

impl ClipboardWatcher {
    pub fn new() -> Self {
        Self::default()
    }

    /// Called only when the OS clipboard change counter has moved. Returns `Some(url)`
    /// the first time a new, non-dismissed, non-repeating well-formed link appears.
    pub fn on_clipboard_change(&mut self, text: Option<&str>) -> Option<String> {
        if !self.primed {
            self.primed = true;
            return None;
        }
        let Some(url) = text.and_then(extract_url) else {
            // Non-link content clears the "currently shown" marker so a later re-copy
            // of the same link is not permanently blocked, but never nags by itself.
            self.last_shown = None;
            return None;
        };
        if self.dismissed.contains(&url) || self.last_shown.as_deref() == Some(url.as_str()) {
            return None;
        }
        self.last_shown = Some(url.clone());
        Some(url)
    }

    /// The user explicitly ignored this suggestion: stop offering this exact link for
    /// the rest of the session.
    pub fn dismiss(&mut self, url: &str) {
        if !url.is_empty() {
            self.dismissed.insert(url.to_owned());
        }
        if self.last_shown.as_deref() == Some(url) {
            self.last_shown = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_a_single_clean_http_or_https_link() {
        assert_eq!(
            extract_url("  https://example.com/files/app.zip  ").as_deref(),
            Some("https://example.com/files/app.zip")
        );
        assert_eq!(
            extract_url("http://host/a").as_deref(),
            Some("http://host/a")
        );
        // Query strings and ports are part of a valid link.
        assert_eq!(
            extract_url("https://cdn.example.com:8443/pkg?token=abc").as_deref(),
            Some("https://cdn.example.com:8443/pkg?token=abc")
        );
    }

    #[test]
    fn ignores_prose_paths_and_multi_line_selections() {
        assert_eq!(extract_url("great app here"), None);
        assert_eq!(extract_url("C:\\Users\\me\\file.zip"), None);
        assert_eq!(extract_url("ftp://files.example.com/a"), None);
        assert_eq!(extract_url("https://"), None);
        assert_eq!(extract_url(""), None);
        assert_eq!(extract_url("https://a.example/one\nhttps://b.example/two"), None);
        assert_eq!(extract_url("Download: https://a.example/f.zip"), None);
        assert_eq!(extract_url("https://a.example/f.zip extra"), None);
    }

    #[test]
    fn strips_surrounding_quotes_and_whitespace() {
        assert_eq!(
            extract_url("\t \"https://a.example/f.zip\"\n ").as_deref(),
            Some("https://a.example/f.zip")
        );
    }

    #[test]
    fn watcher_primes_once_then_surfaces_only_new_links() {
        let mut w = ClipboardWatcher::new();
        // First change after enabling is only a baseline; never re-suggest pre-existing clipboard.
        assert_eq!(w.on_clipboard_change(Some("https://already.example/x")), None);
        // A genuinely new link is surfaced once.
        assert_eq!(
            w.on_clipboard_change(Some("https://example.com/new.zip")).as_deref(),
            Some("https://example.com/new.zip")
        );
        // The same link re-copied consecutively is not re-nagged.
        assert_eq!(w.on_clipboard_change(Some("https://example.com/new.zip")), None);
    }

    #[test]
    fn watcher_treats_non_link_changes_as_clears() {
        let mut w = ClipboardWatcher::new();
        w.on_clipboard_change(Some("baseline")); // prime
        assert_eq!(w.on_clipboard_change(Some("https://a.example/x")).as_deref(), Some("https://a.example/x"));
        // Copied random text clears the shown marker (no suggestion of its own).
        assert_eq!(w.on_clipboard_change(Some("just some notes")), None);
        // Re-copying the earlier link now surfaces again because focus moved away.
        assert_eq!(w.on_clipboard_change(Some("https://a.example/x")).as_deref(), Some("https://a.example/x"));
    }

    #[test]
    fn dismiss_suppresses_that_link_for_the_session() {
        let mut w = ClipboardWatcher::new();
        w.on_clipboard_change(Some("prime"));
        assert_eq!(w.on_clipboard_change(Some("https://skip.example/y")).as_deref(), Some("https://skip.example/y"));
        w.dismiss("https://skip.example/y");
        // Re-copy after dismissal never re-appears.
        assert_eq!(w.on_clipboard_change(Some("https://skip.example/y")), None);
    }

    #[test]
    fn display_name_prefers_last_path_segment() {
        assert_eq!(display_name("https://cdn.example.com/dir/tool.tar.gz"), "tool.tar.gz");
        assert_eq!(display_name("https://example.com/"), "example.com");
        assert_eq!(display_name("https://example.com?a=1"), "example.com");
    }
}

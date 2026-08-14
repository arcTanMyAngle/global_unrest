//! In-app video playback: a platform webview parented as a child of the
//! eframe window.
//!
//! **Why a webview and not a decoder.** Every source we can actually get
//! video from serves it through a player *page*, not a media file: YouTube,
//! Vimeo, TikTok, Telegram's post widget, Bluesky's embed. Turning a watch
//! page into a stream URL is scraping, which this project does not do (see
//! CLAUDE.md's hard rules), so the only legitimate way to play one is to let
//! the provider's own published embed run. A webview is what runs it.
//!
//! **Airspace.** The webview is a native child window, not an egui texture:
//! it paints *over* everything egui draws in its rectangle, ignores egui's
//! z-order, and cannot be tinted, clipped to a rounded rect, or scrolled
//! under. Everything else must therefore be laid out *around* the player
//! rect, never on top of it. `hide` is called whenever the player is not
//! visible so the child window does not linger over a page that has moved on.
//!
//! **Origin matters.** The webview is never navigated straight at a provider's
//! embed URL, and never fed the player page with `NavigateToString`. Both of
//! those give the page an *opaque* origin, and YouTube's embed refuses to start
//! from one — it shows "Error 153 / Video player configuration error". This was
//! confirmed from both ends: the same embed URL in a plain `file://` page
//! (opaque origin) fails in an ordinary browser too, and the same URL served
//! over `http://127.0.0.1` plays. So the player page is served through a `wry`
//! custom protocol, which WebView2 maps onto the real origin
//! `http://<scheme>.localhost`, and the embed runs in an `<iframe>` inside it.
//!
//! **Windows only.** `wry` is declared under
//! `[target.'cfg(windows)'.dependencies]` because WebView2 is preinstalled on
//! Windows 11 while the Linux backend needs webkit2gtk-4.1 dev packages that
//! CI's ubuntu leg has not got. Every other target compiles the stub at the
//! bottom of this file, which renders an honest "open in browser" fallback
//! rather than pretending to play.

use core_types::{Embed, embed_for};

/// A single URL the player can be pointed at.
///
/// Kept separate from the webview so the "what would we play" decision is
/// testable on every platform, including the ones with no webview at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaybackRequest {
    /// The URL as it came from the source — what "open in browser" uses, and
    /// what the user is shown. Never rewritten.
    pub original: String,
    /// The provider embed this maps onto, if any.
    pub embed: Option<Embed>,
}

impl PlaybackRequest {
    pub fn new(original: impl Into<String>) -> Self {
        let original = original.into();
        let embed = embed_for(&original);
        Self { original, embed }
    }

    /// Can this play inside the app, or must it go to the OS browser?
    pub fn is_embeddable(&self) -> bool {
        self.embed.is_some()
    }
}

/// A minimal player page for a direct media file.
///
/// `.m3u8` is included in [`core_types::embed_for`]'s playable extensions
/// because Safari-family webviews play HLS natively — Chromium/WebView2 does
/// not, and there is no bundled hls.js here, so an HLS file shows the
/// `<video>` element's own error state rather than silently blank. The
/// browser fallback stays reachable for exactly that case.
///
/// Only the real webview calls this, so builds without it would warn on an
/// unused function; the test cfg keeps the escaping test running everywhere.
#[cfg(any(test, all(target_os = "windows", feature = "video-embed")))]
fn file_player_html(url: &str) -> String {
    let escaped = escape_attr(url);
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;height:100%;background:#000;\
         display:flex;align-items:center;justify-content:center}}\
         video{{max-width:100%;max-height:100%}}</style>\
         <video src=\"{escaped}\" controls autoplay playsinline></video>"
    )
}

/// A player page that runs a provider's embed in an `<iframe>`.
///
/// The iframe is the whole point — see this module's "Origin matters" note.
/// Navigating the webview at `url` directly would load it with an opaque
/// origin and YouTube would refuse to play.
#[cfg(any(test, all(target_os = "windows", feature = "video-embed")))]
fn page_player_html(url: &str) -> String {
    let escaped = escape_attr(url);
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;height:100%;background:#000;overflow:hidden}}\
         iframe{{border:0;display:block;width:100%;height:100%}}</style>\
         <iframe src=\"{escaped}\" \
         allow=\"autoplay; encrypted-media; picture-in-picture; fullscreen\" \
         referrerpolicy=\"strict-origin-when-cross-origin\" allowfullscreen></iframe>"
    )
}

/// Escape a URL for use inside a double-quoted HTML attribute.
///
/// URLs here come from third-party API responses, so a `"` in one would
/// otherwise break out of the attribute.
#[cfg(any(test, all(target_os = "windows", feature = "video-embed")))]
fn escape_attr(url: &str) -> String {
    url.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(all(target_os = "windows", feature = "video-embed"))]
mod imp {
    use std::borrow::Cow;
    use std::sync::{Arc, Mutex, PoisonError};

    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::raw_window_handle::HasWindowHandle;
    use wry::{Rect, WebView, WebViewBuilder};

    use super::{PlaybackRequest, file_player_html, page_player_html};
    use core_types::Embed;

    /// Custom-protocol name for the player page.
    ///
    /// WebView2 has no real custom-scheme support, so `wry` filters
    /// `http://{SCHEME}.*` instead — which is what gives the page a genuine
    /// http origin (`http://lesplay.localhost`) rather than an opaque one.
    /// Navigation therefore uses that http form directly: the `scheme://`
    /// rewrite only happens for the URL passed at *build* time, not for a
    /// later `load_url`.
    const PLAYER_SCHEME: &str = "lesplay";
    const PLAYER_ORIGIN: &str = "http://lesplay.localhost";

    /// Owns the child webview for the lifetime of the app.
    ///
    /// The webview is built lazily on first play (constructing one costs a
    /// WebView2 environment + a child HWND, which is wasted on every session
    /// that never opens a video) and then reused: `load_url` is far cheaper
    /// than tearing the child window down and rebuilding it, and rebuilding
    /// flickers.
    #[derive(Default)]
    pub struct VideoPlayer {
        webview: Option<WebView>,
        /// What the live webview is currently showing, so an unchanged
        /// request does not reload the page every frame.
        loaded: Option<String>,
        visible: bool,
        /// Set when construction failed, so the failure is reported once
        /// instead of retried every frame.
        error: Option<String>,
        /// The player page the custom protocol serves. Shared with the
        /// protocol handler, which runs on the webview's own callback and so
        /// cannot borrow from `self`.
        page: Arc<Mutex<String>>,
        /// Bumped for every load so each navigation has a distinct URL —
        /// WebView2 would otherwise treat a repeat of the same address as a
        /// no-op and keep showing the previous video.
        nav: u64,
    }

    impl VideoPlayer {
        pub fn new() -> Self {
            Self::default()
        }

        /// Position the player over `rect` (egui points) and make sure it is
        /// showing `request`. Returns `Err` with a human-readable reason if
        /// the platform webview could not be created at all.
        pub fn show(
            &mut self,
            frame: &eframe::Frame,
            rect: egui::Rect,
            pixels_per_point: f32,
            request: &PlaybackRequest,
        ) -> Result<(), String> {
            if let Some(error) = &self.error {
                return Err(error.clone());
            }
            let Some(embed) = request.embed.clone() else {
                return Err("no embeddable player for this link".to_string());
            };

            // egui works in points; the child window is placed in physical
            // pixels. At this machine's 150% scaling the two differ by 1.5,
            // so skipping this conversion puts the video two-thirds of the
            // way to the right size.
            let bounds = Rect {
                position: PhysicalPosition::new(
                    (rect.min.x * pixels_per_point).round() as i32,
                    (rect.min.y * pixels_per_point).round() as i32,
                )
                .into(),
                size: PhysicalSize::new(
                    (rect.width() * pixels_per_point).round().max(1.0) as u32,
                    (rect.height() * pixels_per_point).round().max(1.0) as u32,
                )
                .into(),
            };

            if self.webview.is_none() {
                let nav = self.arm(&embed);
                match Self::build(frame, bounds, &nav, Arc::clone(&self.page)) {
                    Ok(webview) => {
                        self.webview = Some(webview);
                        self.loaded = Some(embed_key(&embed));
                        self.visible = true;
                        return Ok(());
                    }
                    Err(e) => {
                        // WebView2 missing is the realistic failure on a
                        // stripped Windows install; report it once and let
                        // the caller fall back to the browser link.
                        let msg = format!("embedded player unavailable: {e}");
                        self.error = Some(msg.clone());
                        return Err(msg);
                    }
                }
            }

            let key = embed_key(&embed);
            let needs_load = self.loaded.as_deref() != Some(key.as_str());
            let nav = needs_load.then(|| self.arm(&embed));

            let webview = self.webview.as_ref().expect("just checked");
            let _ = webview.set_bounds(bounds);
            if !self.visible {
                let _ = webview.set_visible(true);
                self.visible = true;
            }
            if let Some(nav) = nav {
                if let Err(e) = webview.load_url(&nav) {
                    return Err(format!("could not load player: {e}"));
                }
                self.loaded = Some(key);
            }
            Ok(())
        }

        /// Put `embed`'s player page where the protocol handler will find it,
        /// and return the one-shot URL that fetches it.
        fn arm(&mut self, embed: &Embed) -> String {
            let html = match embed {
                Embed::Page(url) => page_player_html(url),
                Embed::File(url) => file_player_html(url),
            };
            // A poisoned lock here would mean the handler panicked mid-serve;
            // the page string is still perfectly usable, so recover rather
            // than take the whole app down over a video.
            *self.page.lock().unwrap_or_else(PoisonError::into_inner) = html;
            self.nav += 1;
            format!("{PLAYER_ORIGIN}/p{}", self.nav)
        }

        fn build(
            frame: &eframe::Frame,
            bounds: Rect,
            nav_url: &str,
            page: Arc<Mutex<String>>,
        ) -> Result<WebView, Box<dyn std::error::Error>> {
            let handle = frame.window_handle()?;
            let builder = WebViewBuilder::new()
                .with_bounds(bounds)
                // Autoplay is the point: the user already clicked "play" in
                // our UI, so a second click inside the embed is friction.
                .with_autoplay(true)
                .with_background_color((0, 0, 0, 255))
                // Nothing this webview loads should outlive the session:
                // it renders third-party player pages, and we have no reason
                // to keep their cookies on disk afterwards.
                .with_incognito(true)
                // Serves whatever `arm` last put in `page`, at any path under
                // the scheme. The path only ever varies to defeat WebView2's
                // same-URL navigation shortcut, so it is not inspected.
                .with_custom_protocol(PLAYER_SCHEME.to_string(), move |_id, _request| {
                    let html = page
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                        .into_bytes();
                    wry::http::Response::builder()
                        .header(wry::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
                        .body(Cow::Owned(html))
                        .unwrap_or_default()
                })
                .with_url(nav_url);
            Ok(builder.build_as_child(&handle)?)
        }

        /// Stop showing the player. Cheap and idempotent — safe to call every
        /// frame the player is not wanted, which is what keeps the child
        /// window from hanging over an unrelated page.
        pub fn hide(&mut self) {
            if self.visible
                && let Some(webview) = &self.webview
            {
                // Blank the page as well as hiding the window: a hidden
                // webview keeps playing audio otherwise.
                let _ = webview.load_html("<!doctype html><body style=\"background:#000\">");
                let _ = webview.set_visible(false);
                self.loaded = None;
            }
            self.visible = false;
        }
    }

    /// Identity of what is loaded, so `show` can skip a redundant reload.
    fn embed_key(embed: &Embed) -> String {
        match embed {
            Embed::Page(url) | Embed::File(url) => url.clone(),
        }
    }
}

#[cfg(not(all(target_os = "windows", feature = "video-embed")))]
mod imp {
    use super::PlaybackRequest;

    /// Stub player for builds without an embedded webview (any non-Windows
    /// target, or `--no-default-features`). It never claims to play: `show`
    /// always fails with a reason the UI prints next to the browser link.
    #[derive(Default)]
    pub struct VideoPlayer;

    impl VideoPlayer {
        pub fn new() -> Self {
            Self
        }

        pub fn show(
            &mut self,
            _frame: &eframe::Frame,
            _rect: egui::Rect,
            _pixels_per_point: f32,
            _request: &PlaybackRequest,
        ) -> Result<(), String> {
            Err("this build has no embedded player (feature `video-embed`, Windows only)".into())
        }

        pub fn hide(&mut self) {}
    }
}

pub use imp::VideoPlayer;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embeddable_and_browser_only_links_are_distinguished() {
        let yt = PlaybackRequest::new("https://www.youtube.com/watch?v=abc123");
        assert!(yt.is_embeddable());
        // The original is preserved verbatim for the browser fallback.
        assert_eq!(yt.original, "https://www.youtube.com/watch?v=abc123");

        let article = PlaybackRequest::new("https://news.example.org/story");
        assert!(!article.is_embeddable());
    }

    #[test]
    fn page_player_frames_the_embed_rather_than_navigating_to_it() {
        // The iframe is what gives the embed a real parent origin; a plain
        // navigation is what produced YouTube's "Error 153".
        let html = page_player_html("https://www.youtube-nocookie.com/embed/abc?a=1&b=2");
        assert!(html.contains("<iframe"));
        assert!(html.contains("src=\"https://www.youtube-nocookie.com/embed/abc?a=1&amp;b=2\""));
        assert!(html.contains("allowfullscreen"));
    }

    #[test]
    fn file_player_escapes_the_url_it_is_given() {
        let html = file_player_html("https://x.example/a.mp4?a=1&b=\"><script>");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&quot;"));
    }
}

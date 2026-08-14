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
fn file_player_html(url: &str) -> String {
    // The URL is attribute-escaped rather than interpolated raw: it comes
    // from a third-party API response, and a `"` in it would otherwise break
    // out of the attribute.
    let escaped = url
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=\"utf-8\">\
         <style>html,body{{margin:0;height:100%;background:#000;\
         display:flex;align-items:center;justify-content:center}}\
         video{{max-width:100%;max-height:100%}}</style>\
         <video src=\"{escaped}\" controls autoplay playsinline></video>"
    )
}

#[cfg(all(target_os = "windows", feature = "video-embed"))]
mod imp {
    use wry::dpi::{PhysicalPosition, PhysicalSize};
    use wry::raw_window_handle::HasWindowHandle;
    use wry::{Rect, WebView, WebViewBuilder};

    use super::{PlaybackRequest, file_player_html};
    use core_types::Embed;

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
    }

    impl VideoPlayer {
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
                match Self::build(frame, bounds, &embed) {
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

            let webview = self.webview.as_ref().expect("just checked");
            let _ = webview.set_bounds(bounds);
            if !self.visible {
                let _ = webview.set_visible(true);
                self.visible = true;
            }
            let key = embed_key(&embed);
            if self.loaded.as_deref() != Some(key.as_str()) {
                let result = match &embed {
                    Embed::Page(url) => webview.load_url(url),
                    Embed::File(url) => webview.load_html(&file_player_html(url)),
                };
                if let Err(e) = result {
                    return Err(format!("could not load player: {e}"));
                }
                self.loaded = Some(key);
            }
            Ok(())
        }

        fn build(
            frame: &eframe::Frame,
            bounds: Rect,
            embed: &Embed,
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
                .with_incognito(true);
            let builder = match embed {
                Embed::Page(url) => builder.with_url(url),
                Embed::File(url) => builder.with_html(file_player_html(url)),
            };
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
    fn file_player_escapes_the_url_it_is_given() {
        let html = file_player_html("https://x.example/a.mp4?a=1&b=\"><script>");
        assert!(!html.contains("<script>"));
        assert!(html.contains("&amp;"));
        assert!(html.contains("&quot;"));
    }
}

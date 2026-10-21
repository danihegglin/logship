//! User settings shared by every tab: font, font size and theme.
//!
//! They live in a gpui global, so views read them while rendering, and are saved
//! with the session (see `session.rs`).

use std::borrow::Cow;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::theme::Theme;

/// Shipped inside the binary (assets/fonts, SIL Open Font License), so it is
/// available on every platform.
pub const BUNDLED_FONT: &str = "Nordwand Mono";

#[cfg(target_os = "macos")]
pub const SYSTEM_MONO: &str = "Menlo";
#[cfg(target_os = "windows")]
pub const SYSTEM_MONO: &str = "Consolas";
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub const SYSTEM_MONO: &str = "DejaVu Sans Mono";

/// Monospace fonts offered in the settings when they are installed.
const KNOWN_MONO_FONTS: &[&str] = &[
    "JetBrains Mono",
    "Fira Code",
    "Fira Mono",
    "Cascadia Code",
    "Cascadia Mono",
    "SF Mono",
    "Menlo",
    "Monaco",
    "Consolas",
    "Source Code Pro",
    "IBM Plex Mono",
    "Hack",
    "Iosevka",
    "Ubuntu Mono",
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "Roboto Mono",
    "Inconsolata",
];

pub const MIN_FONT_SIZE: f32 = 9.;
pub const MAX_FONT_SIZE: f32 = 24.;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub font_family: String,
    pub font_size: f32,
    pub dark: bool,
    /// Show the newest lines at the top instead of the bottom.
    pub newest_first: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            font_family: BUNDLED_FONT.into(),
            font_size: 12.5,
            dark: true,
            newest_first: false,
        }
    }
}

impl Global for Settings {}

impl Settings {
    /// Height of one log row for the current font size (19 px at 12.5 px).
    pub fn line_height(&self) -> f32 {
        (self.font_size * 1.52).round()
    }

    pub fn theme(&self) -> Theme {
        if self.dark {
            Theme::dark()
        } else {
            Theme::light()
        }
    }

    /// Keeps values loaded from disk within what the UI can show.
    pub fn sanitized(mut self) -> Self {
        if !self.font_size.is_finite() {
            self.font_size = Settings::default().font_size;
        }
        self.font_size = self.font_size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE);
        if self.font_family.trim().is_empty() {
            self.font_family = BUNDLED_FONT.into();
        }
        self
    }
}

/// Registers the bundled font with gpui's text system.
pub fn load_bundled_fonts(cx: &App) {
    let fonts = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/NordwandMono-Regular.ttf").as_slice()),
        Cow::Borrowed(include_bytes!("../assets/fonts/NordwandMono-Bold.ttf").as_slice()),
    ];
    if let Err(err) = cx.text_system().add_fonts(fonts) {
        eprintln!("logship: could not load the bundled font: {err}");
    }
}

/// The fonts to offer: the bundled one, the platform's default monospace font,
/// the current choice, and any known monospace font that is installed.
pub fn font_choices(current: &str, cx: &App) -> Vec<String> {
    let installed = cx.text_system().all_font_names();
    let mut out: Vec<String> = vec![BUNDLED_FONT.into(), SYSTEM_MONO.into()];
    let mut push = |name: &str| {
        if !out.iter().any(|n| n == name) {
            out.push(name.into());
        }
    };
    push(current);
    for name in KNOWN_MONO_FONTS {
        if installed.iter().any(|n| n == name) {
            push(name);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_loaded_values() {
        let s = Settings {
            font_family: " ".into(),
            font_size: 300.,
            dark: false,
            newest_first: true,
        }
        .sanitized();
        assert_eq!(s.font_family, BUNDLED_FONT);
        assert_eq!(s.font_size, MAX_FONT_SIZE);
        assert!(!s.dark);

        let s = Settings {
            font_size: f32::NAN,
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(s.font_size, 12.5);
    }

    #[test]
    fn line_height_matches_the_original_layout() {
        assert_eq!(Settings::default().line_height(), 19.);
    }
}

use gpui::{Hsla, rgb, rgba};

use crate::index::Level;

#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Hsla,
    pub surface: Hsla,
    pub surface_hover: Hsla,
    pub border: Hsla,
    pub text: Hsla,
    pub text_muted: Hsla,
    pub gutter: Hsla,
    pub accent: Hsla,
    pub selection: Hsla,
    pub row_hover: Hsla,
    pub match_bg: Hsla,
    pub current_match_bg: Hsla,
    pub scrollbar_track: Hsla,
    pub scrollbar_thumb: Hsla,
    pub error_text: Hsla,
    levels: [Hsla; Level::COUNT],
}

impl Theme {
    pub fn dark() -> Theme {
        Theme {
            bg: rgb(0x121417).into(),
            surface: rgb(0x1a1d22).into(),
            surface_hover: rgb(0x262a31).into(),
            border: rgb(0x2a2e36).into(),
            text: rgb(0xd4d7dd).into(),
            text_muted: rgb(0x7d8390).into(),
            gutter: rgb(0x4d535e).into(),
            accent: rgb(0x5b9cf5).into(),
            selection: rgba(0x5b9cf533).into(),
            row_hover: rgba(0xffffff0a).into(),
            match_bg: rgba(0xe5c07b55).into(),
            current_match_bg: rgba(0xf0a04bcc).into(),
            scrollbar_track: rgba(0xffffff08).into(),
            scrollbar_thumb: rgba(0xffffff30).into(),
            error_text: rgb(0xf07178).into(),
            levels: [
                rgb(0x4d535e).into(),
                rgb(0x7d8390).into(),
                rgb(0x56b6c2).into(),
                rgb(0x98c379).into(),
                rgb(0xe5c07b).into(),
                rgb(0xef6b73).into(),
            ],
        }
    }

    pub fn light() -> Theme {
        Theme {
            bg: rgb(0xfbfbfc).into(),
            surface: rgb(0xf0f1f3).into(),
            surface_hover: rgb(0xe3e5e9).into(),
            border: rgb(0xd9dce1).into(),
            text: rgb(0x24292f).into(),
            text_muted: rgb(0x6b7280).into(),
            gutter: rgb(0xa0a6b0).into(),
            accent: rgb(0x2f6fdb).into(),
            selection: rgba(0x2f6fdb26).into(),
            row_hover: rgba(0x0000000a).into(),
            match_bg: rgba(0xf5c54277).into(),
            current_match_bg: rgba(0xf08c2bcc).into(),
            scrollbar_track: rgba(0x0000000a).into(),
            scrollbar_thumb: rgba(0x00000033).into(),
            error_text: rgb(0xc9303b).into(),
            levels: [
                rgb(0xa0a6b0).into(),
                rgb(0x8a919c).into(),
                rgb(0x1a8a9a).into(),
                rgb(0x3a8a2e).into(),
                rgb(0xb07800).into(),
                rgb(0xd0303b).into(),
            ],
        }
    }

    pub fn level(&self, level: Level) -> Hsla {
        self.levels[level as usize]
    }
}

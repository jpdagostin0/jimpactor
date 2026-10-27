use iced::{Color, Theme, color};

mod button;
mod fonts;
mod picklist;

pub(crate) use button::{p_button, s_button};
#[allow(unused)]
pub(crate) use fonts::{
    CHEVRON_BACK, DOWNLOAD, FILE, GEAR, MINUS, PLUS, SHARE, STAR, WRENCH, icon, icon_text,
    load_fonts,
};
pub(crate) use picklist::s_pick_list;

pub(crate) const THEME_CORNER_RADIUS: f32 = 4.0;
pub(crate) const THEME_FONT_SIZE: f32 = 12.0;
pub(crate) const THEME_PADDING: f32 = 9.0;
pub(crate) const THEME_ICON_SIZE: f32 = 12.0;

pub(crate) fn p_font() -> iced::Font {
    iced::Font {
        family: iced::font::Family::Name("Hack".into()),
        weight: iced::font::Weight::Normal,
        stretch: iced::font::Stretch::Normal,
        style: iced::font::Style::Normal,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlumeTheme {
    Dark,
    Light,
}

impl PlumeTheme {
    pub(crate) fn to_iced_theme(self) -> Theme {
        match self {
            Self::Dark => Theme::custom(
                "Jimpactor Dark".to_string(),
                iced::theme::Palette {
                    background: color!(0x1e1f22),
                    text: color!(0xe4e4e7),
                    primary: color!(0x3b82f6),
                    success: color!(0x22c55e),
                    danger: color!(0xef4444),
                    warning: color!(0xf59e0b),
                },
            ),
            Self::Light => Theme::custom(
                "Jimpactor Light".to_string(),
                iced::theme::Palette {
                    background: color!(0xf5f5f7),
                    text: color!(0x1c1c1e),
                    primary: color!(0x2563eb),
                    success: color!(0x16a34a),
                    danger: color!(0xdc2626),
                    warning: color!(0xd97706),
                },
            ),
        }
    }
}

impl Default for PlumeTheme {
    fn default() -> Self {
        Self::Dark
    }
}

/// Moves `color` away from the background tone: lighter on dark themes, darker on light ones.
/// Used for surfaces (buttons, pick lists) layered on the window background.
pub(crate) fn raise(theme: &Theme, color: Color, amount: f32) -> Color {
    if theme.extended_palette().is_dark {
        lighten(color, amount)
    } else {
        darken(color, amount)
    }
}

pub(crate) fn lighten(color: Color, amount: f32) -> Color {
    Color {
        r: (color.r + amount).min(1.0),
        g: (color.g + amount).min(1.0),
        b: (color.b + amount).min(1.0),
        a: color.a,
    }
}

pub(crate) fn darken(color: Color, amount: f32) -> Color {
    Color {
        r: (color.r - amount).max(0.0),
        g: (color.g - amount).max(0.0),
        b: (color.b - amount).max(0.0),
        a: color.a,
    }
}

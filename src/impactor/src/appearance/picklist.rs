use iced::widget::pick_list;
use iced::{Background, Border, Color, Theme};

use super::{THEME_CORNER_RADIUS, raise};

/// Secondary pick list style
pub fn s_pick_list(theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let palette = theme.palette();

    match status {
        pick_list::Status::Active => pick_list::Style {
            text_color: palette.text,
            placeholder_color: palette.text.scale_alpha(0.55),
            handle_color: palette.text,
            background: Background::Color(raise(theme, palette.background, 0.05)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
        },
        pick_list::Status::Hovered => pick_list::Style {
            text_color: palette.text,
            placeholder_color: palette.text.scale_alpha(0.65),
            handle_color: palette.text,
            background: Background::Color(raise(theme, palette.background, 0.11)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
        },
        pick_list::Status::Opened { .. } => pick_list::Style {
            text_color: palette.text,
            placeholder_color: palette.text.scale_alpha(0.65),
            handle_color: palette.text,
            background: Background::Color(raise(theme, palette.background, 0.09)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
        },
    }
}

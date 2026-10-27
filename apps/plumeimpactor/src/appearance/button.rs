use iced::widget::button;
use iced::{Background, Border, Color, Shadow, Theme};

use super::{THEME_CORNER_RADIUS, darken, lighten, raise};

/// Primary button style
pub fn p_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();

    match status {
        button::Status::Active => button::Style {
            background: Some(Background::Color(palette.primary)),
            text_color: Color::WHITE,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(lighten(palette.primary, 0.08))),
            text_color: Color::WHITE,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(darken(palette.primary, 0.06))),
            text_color: Color::WHITE,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(
                palette.primary.scale_alpha(0.3),
            )),
            text_color: Color::WHITE.scale_alpha(0.6),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
    }
}

/// Secondary button style
pub fn s_button(theme: &Theme, status: button::Status) -> button::Style {
    let palette = theme.palette();

    match status {
        button::Status::Active => button::Style {
            background: Some(Background::Color(raise(theme, palette.background, 0.05))),
            text_color: palette.text,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Hovered => button::Style {
            background: Some(Background::Color(raise(theme, palette.background, 0.11))),
            text_color: palette.text,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Pressed => button::Style {
            background: Some(Background::Color(raise(theme, palette.background, 0.08))),
            text_color: palette.text,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
        button::Status::Disabled => button::Style {
            background: Some(Background::Color(raise(theme, palette.background, 0.03))),
            text_color: palette.text.scale_alpha(0.4),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: THEME_CORNER_RADIUS.into(),
            },
            shadow: Shadow::default(),
            snap: false,
        },
    }
}

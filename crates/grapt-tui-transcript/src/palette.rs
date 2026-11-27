use ratatui::style::{Color, Style};

/// Calm transcript palette; the light theme retains its established contrast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub light: bool,
    pub background: Color,
    pub panel: Color,
    pub text: Color,
    pub muted: Color,
    pub accent: Color,
    pub good: Color,
    pub bad: Color,
}

impl Palette {
    pub fn new(light: bool) -> Self {
        if light {
            Self {
                light,
                background: Color::Rgb(229, 231, 235),
                panel: Color::Rgb(209, 213, 219),
                text: Color::Rgb(31, 41, 55),
                muted: Color::Rgb(75, 85, 99),
                accent: Color::Rgb(37, 99, 235),
                good: Color::Rgb(21, 128, 61),
                bad: Color::Rgb(185, 28, 28),
            }
        } else {
            Self {
                light,
                background: Color::Rgb(22, 25, 31),
                panel: Color::Rgb(36, 39, 46),
                text: Color::Rgb(222, 222, 219),
                muted: Color::Rgb(150, 153, 159),
                accent: Color::Rgb(37, 99, 235),
                good: Color::Rgb(134, 239, 172),
                bad: Color::Rgb(252, 165, 165),
            }
        }
    }

    pub fn normal(self) -> Style {
        Style::default().fg(self.text).bg(self.background)
    }
}

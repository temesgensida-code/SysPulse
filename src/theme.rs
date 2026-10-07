//! Colour themes. "colorblind" uses the Okabe-Ito palette.

use ratatui::style::Color;

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub dim: Color,
    pub accent: Color,
    pub ok: Color,
    pub warn: Color,
    pub crit: Color,
    pub cpu: Color,
    pub mem: Color,
    pub cache: Color,
    pub rx: Color,
    pub tx: Color,
    pub disk_r: Color,
    pub disk_w: Color,
    pub avg: Color,
    pub sel_bg: Color,
}

impl Theme {
    pub fn by_name(name: &str) -> Theme {
        match name.to_ascii_lowercase().as_str() {
            "light" => Theme {
                bg: Color::White,
                fg: Color::Black,
                dim: Color::Gray,
                accent: Color::Blue,
                ok: Color::Green,
                warn: Color::Rgb(200, 120, 0),
                crit: Color::Red,
                cpu: Color::Blue,
                mem: Color::Magenta,
                cache: Color::Cyan,
                rx: Color::Green,
                tx: Color::Blue,
                disk_r: Color::Cyan,
                disk_w: Color::Magenta,
                avg: Color::DarkGray,
                sel_bg: Color::Rgb(210, 225, 255),
            },
            "colorblind" | "cb" => Theme {
                bg: Color::Reset,
                fg: Color::Reset,
                dim: Color::DarkGray,
                accent: Color::Rgb(86, 180, 233),
                ok: Color::Rgb(0, 114, 178),
                warn: Color::Rgb(240, 228, 66),
                crit: Color::Rgb(213, 94, 0),
                cpu: Color::Rgb(86, 180, 233),
                mem: Color::Rgb(230, 159, 0),
                cache: Color::Rgb(240, 228, 66),
                rx: Color::Rgb(0, 158, 115),
                tx: Color::Rgb(213, 94, 0),
                disk_r: Color::Rgb(0, 114, 178),
                disk_w: Color::Rgb(204, 121, 167),
                avg: Color::DarkGray,
                sel_bg: Color::Rgb(40, 40, 60),
            },
            _ => Theme {
                bg: Color::Reset,
                fg: Color::Reset,
                dim: Color::DarkGray,
                accent: Color::Cyan,
                ok: Color::Green,
                warn: Color::Yellow,
                crit: Color::Red,
                cpu: Color::Cyan,
                mem: Color::Magenta,
                cache: Color::Blue,
                rx: Color::Green,
                tx: Color::Yellow,
                disk_r: Color::LightBlue,
                disk_w: Color::LightMagenta,
                avg: Color::DarkGray,
                sel_bg: Color::Rgb(40, 44, 60),
            },
        }
    }
}

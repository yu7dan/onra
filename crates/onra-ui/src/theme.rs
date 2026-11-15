use std::cmp::Ordering;
use std::env;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    None,
    /// The 16 standard ANSI colors. Universally supported.
    Basic,
    /// 24-bit color, needed for gradients.
    TrueColor,
}

/// Which color means "up". East Asian markets show gains in red and losses
/// in green; Western markets do the opposite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convention {
    East,
    West,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub(crate) fn lerp(self, to: Rgb, t: f32) -> Rgb {
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(mix(self.0, to.0), mix(self.1, to.1), mix(self.2, to.2))
    }
}

/// Each color pairs a truecolor value with its closest basic ANSI code.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Color {
    rgb: Rgb,
    basic: u8,
}

pub(crate) mod palette {
    use super::{Color, Rgb};

    pub const AMBER: Color = Color { rgb: Rgb(245, 158, 11), basic: 33 };
    pub const SUN_TOP: Rgb = Rgb(253, 224, 71);
    pub const SUN_BOTTOM: Rgb = Rgb(234, 88, 12);
    pub const RED: Color = Color { rgb: Rgb(239, 68, 68), basic: 31 };
    pub const GREEN: Color = Color { rgb: Rgb(34, 197, 94), basic: 32 };
    pub const GRAY: Color = Color { rgb: Rgb(115, 115, 115), basic: 90 };
    pub const WARN: Color = Color { rgb: Rgb(250, 204, 21), basic: 93 };
}

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    pub mode: ColorMode,
    pub convention: Convention,
}

impl Theme {
    pub fn plain(convention: Convention) -> Self {
        Self { mode: ColorMode::None, convention }
    }

    /// Detect what stdout supports. Callers decide separately whether stdout
    /// is a terminal at all.
    pub fn detect(convention: Convention) -> Self {
        Self { mode: detect_mode(), convention }
    }

    fn paint(&self, text: &str, color: Color) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            ColorMode::Basic => format!("\x1b[{}m{text}\x1b[0m", color.basic),
            ColorMode::TrueColor => {
                let Rgb(r, g, b) = color.rgb;
                format!("\x1b[38;2;{r};{g};{b}m{text}\x1b[0m")
            }
        }
    }

    pub(crate) fn rgb(&self, text: &str, rgb: Rgb, basic: u8) -> String {
        self.paint(text, Color { rgb, basic })
    }

    pub fn brand(&self, text: &str) -> String {
        self.paint(text, palette::AMBER)
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(text, palette::GRAY)
    }

    pub fn warn(&self, text: &str) -> String {
        self.paint(text, palette::WARN)
    }

    pub fn bold(&self, text: &str) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            _ => format!("\x1b[1m{text}\x1b[0m"),
        }
    }

    /// Color by price direction, honoring the up/down convention.
    pub fn movement(&self, text: &str, direction: Ordering) -> String {
        let (up, down) = match self.convention {
            Convention::East => (palette::RED, palette::GREEN),
            Convention::West => (palette::GREEN, palette::RED),
        };
        match direction {
            Ordering::Greater => self.paint(text, up),
            Ordering::Less => self.paint(text, down),
            Ordering::Equal => self.dim(text),
        }
    }
}

fn detect_mode() -> ColorMode {
    if env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return ColorMode::None;
    }
    // Legacy Windows consoles need VT processing switched on explicitly.
    if cfg!(windows) && enable_ansi_support::enable_ansi_support().is_err() {
        return ColorMode::None;
    }
    let colorterm = env::var("COLORTERM").unwrap_or_default();
    let term_program = env::var("TERM_PROGRAM").unwrap_or_default();
    let truecolor = matches!(colorterm.as_str(), "truecolor" | "24bit")
        || env::var_os("WT_SESSION").is_some()
        || matches!(
            term_program.as_str(),
            "vscode" | "iTerm.app" | "WezTerm" | "ghostty" | "Apple_Terminal"
        );
    if truecolor { ColorMode::TrueColor } else { ColorMode::Basic }
}

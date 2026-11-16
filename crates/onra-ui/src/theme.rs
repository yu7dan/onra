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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn lerp(self, to: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let mix = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(mix(self.0, to.0), mix(self.1, to.1), mix(self.2, to.2))
    }

    /// Fade toward the terminal background. `alpha` 0 is invisible, 1 is
    /// full color. Assumes a dark background, which nearly every terminal
    /// theme for trading uses.
    pub fn fade(self, alpha: f32) -> Rgb {
        palette::BACKGROUND.lerp(self, alpha)
    }

    /// The closest of the 16 standard ANSI foreground codes.
    pub fn nearest_basic(self) -> u8 {
        const BASIC: [(u8, Rgb); 12] = [
            (30, Rgb(0, 0, 0)),
            (31, Rgb(205, 49, 49)),
            (32, Rgb(13, 188, 121)),
            (33, Rgb(229, 160, 16)),
            (37, Rgb(204, 204, 204)),
            (90, Rgb(102, 102, 102)),
            (91, Rgb(241, 76, 76)),
            (92, Rgb(35, 209, 139)),
            (93, Rgb(245, 222, 80)),
            (95, Rgb(214, 112, 214)),
            (96, Rgb(41, 184, 219)),
            (97, Rgb(242, 242, 242)),
        ];
        let dist = |c: Rgb| {
            let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
            d(self.0, c.0) + d(self.1, c.1) + d(self.2, c.2)
        };
        BASIC.iter().min_by_key(|(_, c)| dist(*c)).map(|(code, _)| *code).unwrap_or(37)
    }
}

/// Each color pairs a truecolor value with its closest basic ANSI code.
#[derive(Debug, Clone, Copy)]
pub struct Color {
    pub rgb: Rgb,
    pub basic: u8,
}

pub mod palette {
    use super::{Color, Rgb};

    pub const BACKGROUND: Rgb = Rgb(13, 13, 16);
    /// The one accent color.
    pub const AMBER: Color = Color { rgb: Rgb(245, 158, 11), basic: 33 };
    pub const SUN_TOP: Rgb = Rgb(253, 224, 71);
    /// The sun's hot core and its cooler rim.
    pub const SUN_CORE: Rgb = Rgb(255, 238, 170);
    pub const SUN_RIM: Rgb = Rgb(244, 114, 22);
    /// Warm off-white for primary text; pure white reads harsh on dark.
    pub const PAPER: Rgb = Rgb(246, 240, 228);
    pub const HORIZON: Rgb = Rgb(120, 110, 100);
    /// Secondary text, then tertiary text and rules.
    pub const MUTED: Rgb = Rgb(110, 110, 118);
    pub const FAINT: Rgb = Rgb(72, 72, 80);
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

    pub fn paint(&self, text: &str, color: Color) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            ColorMode::Basic => format!("[{}m{text}[0m", color.basic),
            ColorMode::TrueColor => {
                let Rgb(r, g, b) = color.rgb;
                format!("[38;2;{r};{g};{b}m{text}[0m")
            }
        }
    }

    /// Paint with an arbitrary color; basic terminals get the nearest ANSI one.
    pub fn rgb(&self, text: &str, rgb: Rgb) -> String {
        self.paint(text, Color { rgb, basic: rgb.nearest_basic() })
    }

    /// `text` in `fg` on a `bg` background, e.g. a price flash. Basic
    /// terminals get reverse video instead.
    pub fn on(&self, text: &str, fg: Rgb, bg: Rgb) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            ColorMode::Basic => format!("[7m{text}[0m"),
            ColorMode::TrueColor => format!(
                "[38;2;{};{};{};48;2;{};{};{}m{text}[0m",
                fg.0, fg.1, fg.2, bg.0, bg.1, bg.2
            ),
        }
    }

    pub fn brand(&self, text: &str) -> String {
        self.paint(text, palette::AMBER)
    }

    /// Primary text: warm off-white, bold.
    pub fn strong(&self, text: &str) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            _ => self.bold(&self.rgb(text, palette::PAPER)),
        }
    }

    pub fn dim(&self, text: &str) -> String {
        self.paint(text, palette::GRAY)
    }

    pub fn faint(&self, text: &str) -> String {
        self.rgb(text, palette::FAINT)
    }

    pub fn warn(&self, text: &str) -> String {
        self.paint(text, palette::WARN)
    }

    /// Errors and refusals. Always red: unlike price moves, failure does not
    /// change color with the market convention.
    pub fn error(&self, text: &str) -> String {
        self.paint(text, palette::RED)
    }

    pub fn bold(&self, text: &str) -> String {
        match self.mode {
            ColorMode::None => text.to_string(),
            _ => format!("[1m{text}[0m"),
        }
    }

    /// The color for a price direction, honoring the up/down convention.
    pub fn movement_color(&self, direction: Ordering) -> Color {
        let (up, down) = match self.convention {
            Convention::East => (palette::RED, palette::GREEN),
            Convention::West => (palette::GREEN, palette::RED),
        };
        match direction {
            Ordering::Greater => up,
            Ordering::Less => down,
            Ordering::Equal => palette::GRAY,
        }
    }

    pub fn movement(&self, text: &str, direction: Ordering) -> String {
        self.paint(text, self.movement_color(direction))
    }

    /// A band of light sweeping across `text`; `t` in `[0, 1)` is one pass.
    /// Used for "working" labels. Without truecolor it is plain brand color.
    pub fn shimmer(&self, text: &str, t: f32) -> String {
        if self.mode != ColorMode::TrueColor {
            return self.brand(text);
        }
        let n = text.chars().count() as f32;
        // The band travels past both ends so it enters and leaves cleanly.
        let center = -4.0 + t.fract() * (n + 8.0);
        let mut out = String::new();
        for (i, ch) in text.chars().enumerate() {
            let glow = (1.0 - (i as f32 - center).abs() / 4.0).max(0.0);
            let Rgb(r, g, b) = palette::AMBER.rgb.lerp(palette::SUN_CORE, glow);
            out.push_str(&format!("[38;2;{r};{g};{b}m{ch}"));
        }
        out.push_str("[0m");
        out
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

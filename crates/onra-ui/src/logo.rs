//! The Onra mark: a sun rising over the horizon, with rays and a shimmering
//! reflection on the water. Drawn procedurally so it can animate.
//!
//! Every frame is a pure function of `t` in `[0, 1]`: 0 is the first frame of
//! the intro, 1 the resting state, so the animation always lands exactly on
//! the static logo.

use std::f32::consts::PI;

use crate::canvas::Canvas;
use crate::theme::{ColorMode, Rgb, palette};

const EMBLEM_W: usize = 41;
const CX: f32 = 20.0;
const R: f32 = 9.5;
const HORIZON_Y: i32 = 13;

fn ease_out(t: f32) -> f32 {
    1.0 - (1.0 - t.clamp(0.0, 1.0)).powi(3)
}

/// Remap `t` so that `[from, to]` becomes `[0, 1]`.
fn phase(t: f32, from: f32, to: f32) -> f32 {
    ((t - from) / (to - from)).clamp(0.0, 1.0)
}

/// The full lockup: emblem, wordmark, tagline, venues, version. Returns
/// terminal lines, left-padded by `indent` spaces. Every frame has the same
/// number of lines.
pub fn logo(t: f32, mode: ColorMode, indent: usize, version: &str) -> Vec<String> {
    let center = |width: usize| " ".repeat(indent + EMBLEM_W.saturating_sub(width) / 2);

    let mut lines: Vec<String> =
        emblem(t).render(mode).into_iter().map(|l| format!("{}{l}", center(EMBLEM_W))).collect();

    let word = wordmark(ease_out(phase(t, 0.45, 0.9)));
    lines.push(String::new());
    lines.extend(word.render(mode).into_iter().map(|l| format!("{}{l}", center(word.width()))));
    lines.push(String::new());

    // The tagline types itself out, then venues and version fade in.
    let tagline = "M A R K E T S   N E V E R   S L E E P";
    let shown = (tagline.chars().count() as f32 * phase(t, 0.6, 0.95)).round() as usize;
    let typed: String = tagline.chars().take(shown).collect();
    lines.push(format!(
        "{}{}",
        center(tagline.chars().count()),
        paint(&typed, palette::HORIZON, mode)
    ));

    let alpha = phase(t, 0.85, 1.0);
    let venues = "binance · hyperliquid · jupiter";
    let dot = paint("●", palette::AMBER.rgb.fade(alpha), mode);
    let text = paint(venues, palette::MUTED.fade(alpha), mode);
    lines.push(format!("{}{dot} {text}", center(venues.chars().count() + 2)));

    let version = format!("v{version}");
    lines.push(format!(
        "{}{}",
        center(version.len()),
        paint(&version, palette::FAINT.fade(alpha), mode)
    ));
    lines
}

fn paint(text: &str, c: Rgb, mode: ColorMode) -> String {
    match mode {
        ColorMode::None => text.to_string(),
        ColorMode::Basic => format!("\x1b[{}m{text}\x1b[0m", c.nearest_basic()),
        ColorMode::TrueColor => format!("\x1b[38;2;{};{};{}m{text}\x1b[0m", c.0, c.1, c.2),
    }
}

/// Whether a pixel center lies in a disc of radius `r`. Rows whose chord
/// would be a single pixel are dropped: lone nubs on top of a round shape
/// read as noise.
fn in_disc(dx: f32, dy: f32, r: f32) -> bool {
    dx * dx + dy * dy <= r * r && r * r - dy * dy >= 2.0
}

fn emblem(t: f32) -> Canvas {
    let mut c = Canvas::new(EMBLEM_W, 20);
    let rise = ease_out(phase(t, 0.0, 0.65));
    let cy = HORIZON_Y as f32 + (R + 1.0) * (1.0 - rise);

    // The disc, clipped at the horizon, hot core to cooler rim.
    for y in 0..HORIZON_Y {
        for x in 0..EMBLEM_W as i32 {
            let (dx, dy) = (x as f32 - CX, y as f32 + 0.5 - cy);
            if in_disc(dx, dy, R) {
                let d = (dx * dx + dy * dy).sqrt() / R;
                c.set(x, y, palette::SUN_CORE.lerp(palette::SUN_RIM, d.powf(1.6)));
            }
        }
    }

    // Five rays, evenly spaced across the sky, fading in once risen.
    let rays = phase(t, 0.55, 0.9);
    for i in 0..5 {
        let angle = PI * (i as f32 + 1.0) / 6.0;
        let (dx, dy) = (angle.cos(), -angle.sin());
        for step in 0..2 {
            let r = R + 2.5 + step as f32 * 1.2;
            let (x, y) = (CX + dx * r, cy + dy * r);
            if y < HORIZON_Y as f32 - 1.0 {
                let a = rays * (1.0 - step as f32 * 0.35);
                c.set_faded(x.round() as i32, y.floor() as i32, palette::SUN_TOP, a);
            }
        }
    }

    // The horizon fades out toward both ends.
    let line = phase(t, 0.0, 0.3);
    for x in 0..EMBLEM_W as i32 {
        let edge = 1.0 - ((x as f32 - CX).abs() / CX).powi(2);
        c.set_faded(x, HORIZON_Y, palette::HORIZON, edge * line);
    }

    // Reflection: broken gold streaks that narrow and cool with depth. The
    // gap pattern shifts with `t`, so the water shimmers as the sun rises.
    for (k, depth) in [(2, 0.0), (4, 0.5), (6, 1.0)] {
        let y = HORIZON_Y + k;
        let half = R * (0.85 - depth * 0.45) * rise;
        let alpha = rise * (0.9 - depth * 0.5);
        let shift = (t * 12.0) as i32 + k;
        for x in (CX - half).round() as i32..=(CX + half).round() as i32 {
            if (x + shift).rem_euclid(6) != 0 {
                c.set_faded(x, y, palette::SUN_TOP.lerp(palette::SUN_RIM, depth), alpha);
            }
        }
    }
    c
}

/// Pixel font for the wordmark. Every letter of "onra" sits on the x-height,
/// so the word is one compact band; strokes are two pixels wide and corners
/// are rounded to echo the sun.
const GLYPHS: [[&str; 6]; 4] = [
    [".####.", "##..##", "##..##", "##..##", "##..##", ".####."],
    [".####.", "##..##", "##..##", "##..##", "##..##", "##..##"],
    [".####", "##...", "##...", "##...", "##...", "##..."],
    [".#####", "##..##", "##..##", "##..##", "##..##", ".#####"],
];

fn wordmark(alpha: f32) -> Canvas {
    const GAP: usize = 2;
    let width = GLYPHS.iter().map(|g| g[0].len()).sum::<usize>() + GAP * (GLYPHS.len() - 1);
    let mut c = Canvas::new(width, 6);
    let mut x0 = 0;
    for glyph in GLYPHS {
        for (y, row) in glyph.iter().enumerate() {
            for (dx, ch) in row.chars().enumerate() {
                if ch == '#' {
                    let x = x0 + dx;
                    // A whisper of warmth from left to right.
                    let warm = x as f32 / width as f32 * 0.6;
                    c.set_faded(
                        x as i32,
                        y as i32,
                        palette::PAPER.lerp(Rgb(250, 214, 165), warm),
                        alpha,
                    );
                }
            }
        }
        x0 += glyph[0].len() + GAP;
    }
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_keep_their_shape() {
        let first = logo(0.0, ColorMode::None, 2, "0.1.0");
        let last = logo(1.0, ColorMode::None, 2, "0.1.0");
        assert_eq!(first.len(), last.len());
    }
}

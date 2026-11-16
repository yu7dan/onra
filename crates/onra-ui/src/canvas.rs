//! A pixel canvas rendered with half-block characters.
//!
//! Each terminal cell shows two vertically stacked pixels: the upper one as
//! the foreground of `▀`, the lower one as its background. Terminal cells are
//! roughly twice as tall as wide, so these pixels come out square, and every
//! pixel gets its own color: smooth gradients instead of banded rows.

use crate::theme::{ColorMode, Rgb};

pub struct Canvas {
    width: usize,
    height: usize,
    pixels: Vec<Option<Rgb>>,
}

impl Canvas {
    /// `height` is in pixels and is rounded up to an even number.
    pub fn new(width: usize, height: usize) -> Self {
        let height = height + height % 2;
        Self { width, height, pixels: vec![None; width * height] }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn set(&mut self, x: i32, y: i32, color: Rgb) {
        if x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height {
            self.pixels[y as usize * self.width + x as usize] = Some(color);
        }
    }

    /// Set a pixel faded toward the background. Nearly invisible pixels are
    /// skipped so they don't smudge terminals with a non-black background.
    pub fn set_faded(&mut self, x: i32, y: i32, color: Rgb, alpha: f32) {
        if alpha >= 0.1 {
            self.set(x, y, color.fade(alpha));
        }
    }

    pub fn get(&self, x: usize, y: usize) -> Option<Rgb> {
        self.pixels[y * self.width + x]
    }

    /// One string per terminal line, each exactly `width` cells wide.
    pub fn render(&self, mode: ColorMode) -> Vec<String> {
        (0..self.height / 2)
            .map(|row| {
                let mut line = String::new();
                for x in 0..self.width {
                    let top = self.get(x, row * 2);
                    let bottom = self.get(x, row * 2 + 1);
                    push_cell(&mut line, top, bottom, mode);
                }
                if mode != ColorMode::None {
                    line.push_str("\x1b[0m");
                }
                line
            })
            .collect()
    }
}

fn push_cell(out: &mut String, top: Option<Rgb>, bottom: Option<Rgb>, mode: ColorMode) {
    let (glyph, fg, bg) = match (top, bottom) {
        (None, None) => {
            if mode != ColorMode::None {
                out.push_str("\x1b[0m");
            }
            out.push(' ');
            return;
        }
        (Some(t), None) => ('▀', t, None),
        (None, Some(b)) => ('▄', b, None),
        (Some(t), Some(b)) if t == b => ('█', t, None),
        (Some(t), Some(b)) => ('▀', t, Some(b)),
    };
    match mode {
        ColorMode::None => {}
        ColorMode::TrueColor => {
            let Rgb(r, g, b) = fg;
            out.push_str(&format!("\x1b[0;38;2;{r};{g};{b}m"));
            if let Some(Rgb(r, g, b)) = bg {
                out.push_str(&format!("\x1b[48;2;{r};{g};{b}m"));
            }
        }
        ColorMode::Basic => {
            out.push_str(&format!("\x1b[0;{}m", fg.nearest_basic()));
            if let Some(bg) = bg {
                out.push_str(&format!("\x1b[{}m", bg.nearest_basic() + 10));
            }
        }
    }
    out.push(glyph);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_two_pixels_per_cell() {
        let mut c = Canvas::new(3, 2);
        let red = Rgb(255, 0, 0);
        c.set(0, 0, red);
        c.set(1, 1, red);
        c.set(2, 0, red);
        c.set(2, 1, red);
        assert_eq!(c.render(ColorMode::None), vec!["▀▄█".to_string()]);
    }

    #[test]
    fn rounds_height_up() {
        assert_eq!(Canvas::new(1, 3).render(ColorMode::None).len(), 2);
    }
}

//! Motion in the normal terminal flow (no alternate screen).
//!
//! Motion rules: short (under a second), never in the way (skipped when
//! output isn't a terminal, under CI, or with `ONRA_NO_MOTION`), and every
//! animation ends on a frame identical to the static render.

use std::env;
use std::io::{IsTerminal, Write};
use std::thread;
use std::time::{Duration, Instant};

const FPS: u32 = 40;

pub fn motion_enabled() -> bool {
    std::io::stdout().is_terminal()
        && env::var_os("ONRA_NO_MOTION").is_none()
        && env::var_os("CI").is_none()
}

/// Play `frame(t)` for `t` from 0 to 1 over `duration`, redrawing in place.
/// Without motion, only the final frame is printed.
pub fn play(duration: Duration, frame: impl Fn(f32) -> Vec<String>) {
    if !motion_enabled() {
        println!("{}", frame(1.0).join("\n"));
        return;
    }
    let mut live = Live::new();
    let start = Instant::now();
    let tick = Duration::from_secs(1) / FPS;
    loop {
        let t = (start.elapsed().as_secs_f32() / duration.as_secs_f32()).min(1.0);
        live.draw(&frame(t));
        if t >= 1.0 {
            break;
        }
        thread::sleep(tick);
    }
    live.finish();
}

/// Print `lines` one after another, a few milliseconds apart, so a result
/// unfolds instead of appearing as a block.
pub fn reveal(lines: &[String]) {
    let mut out = std::io::stdout().lock();
    let motion = motion_enabled();
    for line in lines {
        let _ = writeln!(out, "{line}");
        if motion && !line.is_empty() {
            let _ = out.flush();
            thread::sleep(Duration::from_millis(14));
        }
    }
    let _ = out.flush();
}

/// A block of lines that can be redrawn in place, e.g. a progress display.
/// The cursor is hidden while it is live.
pub struct Live {
    drawn: usize,
}

impl Live {
    pub fn new() -> Self {
        print!("\x1b[?25l");
        Self { drawn: 0 }
    }

    pub fn draw(&mut self, lines: &[String]) {
        let mut out = std::io::stdout().lock();
        let mut buf = String::new();
        if self.drawn > 0 {
            buf.push_str(&format!("\x1b[{}A", self.drawn));
        }
        for line in lines {
            buf.push_str(&format!("\r\x1b[2K{line}\n"));
        }
        // A shorter frame must not leave the previous frame's tail behind.
        for _ in lines.len()..self.drawn {
            buf.push_str("\r\x1b[2K\n");
        }
        let extra = self.drawn.saturating_sub(lines.len());
        if extra > 0 {
            buf.push_str(&format!("\x1b[{extra}A"));
        }
        let _ = out.write_all(buf.as_bytes());
        let _ = out.flush();
        self.drawn = lines.len();
    }

    /// Erase the block and leave the cursor where it started.
    pub fn clear(mut self) {
        self.draw(&[]);
    }

    /// Keep the last frame on screen.
    pub fn finish(self) {}
}

impl Default for Live {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        print!("\x1b[?25h");
        let _ = std::io::stdout().flush();
    }
}

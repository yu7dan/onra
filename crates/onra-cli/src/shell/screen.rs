//! The bottom-anchored panel. Output scrolls up above it like a normal
//! terminal session; the panel (status, menu, input, hints) stays at the
//! bottom and is redrawn in place. Lines are cut to the terminal width so
//! nothing wraps: wrapping would break the line accounting.

use std::io::Write;

use onra_ui::text;

pub struct Frame {
    pub lines: Vec<String>,
    /// (row within the frame, column) for the text cursor, if shown.
    pub cursor: Option<(usize, usize)>,
}

#[derive(Default)]
pub struct Screen {
    drawn: usize,
    cursor_row: usize,
}

fn terminal_width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

impl Screen {
    /// Move to the first line of the frame and clear everything below.
    fn to_top(&self, buf: &mut String) {
        if self.drawn > 0 && self.cursor_row > 0 {
            buf.push_str(&format!("\x1b[{}A", self.cursor_row));
        }
        buf.push_str("\r\x1b[J");
    }

    fn write_frame(&mut self, buf: &mut String, frame: &Frame) {
        let width = terminal_width().saturating_sub(1).max(20);
        for (i, line) in frame.lines.iter().enumerate() {
            if i > 0 {
                buf.push_str("\r\n");
            }
            buf.push_str(&text::truncate(line, width));
        }
        self.drawn = frame.lines.len();
        let last = self.drawn.saturating_sub(1);
        match frame.cursor {
            Some((row, col)) => {
                let up = last.saturating_sub(row);
                if up > 0 {
                    buf.push_str(&format!("\x1b[{up}A"));
                }
                buf.push_str(&format!("\x1b[{}G\x1b[?25h", col + 1));
                self.cursor_row = row.min(last);
            }
            None => {
                buf.push_str("\x1b[?25l");
                self.cursor_row = last;
            }
        }
    }

    pub fn draw(&mut self, frame: &Frame) {
        let mut buf = String::from("\x1b[?25l");
        self.to_top(&mut buf);
        self.write_frame(&mut buf, frame);
        flush(&buf);
    }

    /// Print `lines` into the scrollback above the frame, then redraw it.
    pub fn print_above(&mut self, lines: &[String], frame: &Frame) {
        let mut buf = String::from("\x1b[?25l");
        self.to_top(&mut buf);
        for line in lines {
            buf.push_str(line);
            buf.push_str("\x1b[0m\r\n");
        }
        self.write_frame(&mut buf, frame);
        flush(&buf);
    }

    /// Remove the frame, leaving the cursor where it began.
    pub fn erase(&mut self) {
        let mut buf = String::new();
        self.to_top(&mut buf);
        buf.push_str("\x1b[?25h");
        self.drawn = 0;
        self.cursor_row = 0;
        flush(&buf);
    }

    /// After a resize the old frame may have rewrapped; start clean.
    pub fn reset(&mut self) {
        flush("\x1b[2J\x1b[H");
        self.drawn = 0;
        self.cursor_row = 0;
    }
}

fn flush(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(s.as_bytes());
    let _ = out.flush();
}

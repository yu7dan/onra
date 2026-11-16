//! A single-line editor with persistent history.

use std::fs;
use std::path::PathBuf;

use unicode_width::UnicodeWidthChar;

const HISTORY_LIMIT: usize = 500;

#[derive(Default)]
pub struct Editor {
    buf: Vec<char>,
    cursor: usize,
    history: Vec<String>,
    /// Position while browsing history, and the draft it replaced.
    browsing: Option<(usize, String)>,
}

fn history_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("onra").join("history"))
}

impl Editor {
    pub fn with_history() -> Self {
        let history = history_path()
            .and_then(|p| fs::read_to_string(p).ok())
            .map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect())
            .unwrap_or_default();
        Self { history, ..Self::default() }
    }

    pub fn text(&self) -> String {
        self.buf.iter().collect()
    }

    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    pub fn at_end(&self) -> bool {
        self.cursor == self.buf.len()
    }

    pub fn set(&mut self, text: &str) {
        self.buf = text.chars().collect();
        self.cursor = self.buf.len();
    }

    pub fn clear(&mut self) {
        self.set("");
        self.browsing = None;
    }

    pub fn insert(&mut self, ch: char) {
        self.buf.insert(self.cursor, ch);
        self.cursor += 1;
        self.browsing = None;
    }

    pub fn backspace(&mut self) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.buf.remove(self.cursor);
        }
    }

    pub fn delete(&mut self) {
        if self.cursor < self.buf.len() {
            self.buf.remove(self.cursor);
        }
    }

    pub fn delete_word(&mut self) {
        while self.cursor > 0 && self.buf[self.cursor - 1] == ' ' {
            self.backspace();
        }
        while self.cursor > 0 && self.buf[self.cursor - 1] != ' ' {
            self.backspace();
        }
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.buf.len());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.buf.len();
    }

    /// Display columns before the cursor.
    pub fn cursor_col(&self) -> usize {
        self.buf[..self.cursor].iter().map(|c| c.width().unwrap_or(0)).sum()
    }

    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let pos = match &self.browsing {
            Some((pos, _)) => pos.saturating_sub(1),
            None => self.history.len() - 1,
        };
        let draft = self.browsing.take().map(|(_, d)| d).unwrap_or_else(|| self.text());
        self.set(&self.history[pos].clone());
        self.browsing = Some((pos, draft));
    }

    pub fn history_next(&mut self) {
        let Some((pos, draft)) = self.browsing.take() else {
            return;
        };
        if pos + 1 < self.history.len() {
            self.set(&self.history[pos + 1].clone());
            self.browsing = Some((pos + 1, draft));
        } else {
            self.set(&draft);
        }
    }

    /// The most recent history entry extending the current text, for inline
    /// suggestions.
    pub fn history_match(&self) -> Option<&str> {
        let text = self.text();
        if text.is_empty() {
            return None;
        }
        self.history
            .iter()
            .rev()
            .find(|h| h.len() > text.len() && h.starts_with(&text))
            .map(|s| s.as_str())
    }

    pub fn commit(&mut self, line: &str) {
        self.browsing = None;
        if line.trim().is_empty() || self.history.last().is_some_and(|l| l == line) {
            return;
        }
        self.history.push(line.to_string());
        if self.history.len() > HISTORY_LIMIT {
            self.history.drain(..self.history.len() - HISTORY_LIMIT);
        }
        if let Some(path) = history_path() {
            if let Some(dir) = path.parent() {
                let _ = fs::create_dir_all(dir);
            }
            let _ = fs::write(path, self.history.join("\n") + "\n");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_and_browses_history() {
        let mut e = Editor::default();
        for c in "price sol".chars() {
            e.insert(c);
        }
        e.delete_word();
        assert_eq!(e.text(), "price ");
        e.history = vec!["price SOL".into(), "price ETH".into()];
        e.history_prev();
        assert_eq!(e.text(), "price ETH");
        e.history_prev();
        assert_eq!(e.text(), "price SOL");
        e.history_next();
        e.history_next();
        assert_eq!(e.text(), "price ");
    }

    #[test]
    fn measures_wide_chars() {
        let mut e = Editor::default();
        e.insert('查');
        e.insert('a');
        assert_eq!(e.cursor_col(), 3);
    }
}

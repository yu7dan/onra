use unicode_width::UnicodeWidthStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

/// A cell keeps its plain text for width math alongside the styled text
/// that is actually printed; ANSI escapes have no display width.
#[derive(Debug, Clone)]
pub struct Cell {
    plain: String,
    styled: String,
}

impl Cell {
    pub fn new(plain: impl Into<String>, styled: impl Into<String>) -> Self {
        Self { plain: plain.into(), styled: styled.into() }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        let text = text.into();
        Self { styled: text.clone(), plain: text }
    }
}

enum Row {
    Cells(Vec<Cell>),
    Gap,
}

pub struct Table {
    columns: Vec<(Cell, Align)>,
    rows: Vec<Row>,
}

const INDENT: &str = "  ";
const COLUMN_GAP: &str = "   ";

impl Table {
    pub fn new(columns: Vec<(Cell, Align)>) -> Self {
        Self { columns, rows: Vec::new() }
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        debug_assert_eq!(cells.len(), self.columns.len());
        self.rows.push(Row::Cells(cells));
    }

    /// A blank line between groups of related rows.
    pub fn gap(&mut self) {
        self.rows.push(Row::Gap);
    }

    pub fn render(&self) -> String {
        let mut widths: Vec<usize> = self.columns.iter().map(|(c, _)| c.plain.width()).collect();
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                for (w, cell) in widths.iter_mut().zip(cells) {
                    *w = (*w).max(cell.plain.width());
                }
            }
        }

        let line = |cells: &mut dyn Iterator<Item = &Cell>| {
            let mut out = String::from(INDENT);
            for (i, cell) in cells.enumerate() {
                if i > 0 {
                    out.push_str(COLUMN_GAP);
                }
                let pad = " ".repeat(widths[i] - cell.plain.width());
                match self.columns[i].1 {
                    Align::Left => {
                        out.push_str(&cell.styled);
                        out.push_str(&pad);
                    }
                    Align::Right => {
                        out.push_str(&pad);
                        out.push_str(&cell.styled);
                    }
                }
            }
            out.trim_end().to_string()
        };

        let mut out = line(&mut self.columns.iter().map(|(c, _)| c));
        out.push('\n');
        for row in &self.rows {
            if let Row::Cells(cells) = row {
                out.push_str(&line(&mut cells.iter()));
            }
            out.push('\n');
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns() {
        let mut t =
            Table::new(vec![(Cell::plain("A"), Align::Left), (Cell::plain("PRICE"), Align::Right)]);
        t.row(vec![Cell::plain("SOL"), Cell::plain("71.39")]);
        t.gap();
        t.row(vec![Cell::new("BTC", "\x1b[1mBTC\x1b[0m"), Cell::plain("95,000.00")]);
        assert_eq!(
            t.render(),
            "  A         PRICE\n  SOL       71.39\n\n  \x1b[1mBTC\x1b[0m   95,000.00\n"
        );
    }
}

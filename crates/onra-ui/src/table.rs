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

    pub fn empty() -> Self {
        Self::plain("")
    }
}

enum Row<'a> {
    Cells(Vec<Cell>),
    Gap,
    /// A full-width line rendered from the table's final width, for headers
    /// and footers that align with the columns.
    Line(Box<dyn Fn(usize) -> String + 'a>),
}

/// Column-aligned rows with optional full-width lines in between.
pub struct Table<'a> {
    aligns: Vec<Align>,
    header: Option<Vec<Cell>>,
    rows: Vec<Row<'a>>,
    indent: usize,
    gap: usize,
}

impl<'a> Table<'a> {
    pub fn new(aligns: Vec<Align>) -> Self {
        Self { aligns, header: None, rows: Vec::new(), indent: 2, gap: 3 }
    }

    pub fn header(mut self, cells: Vec<Cell>) -> Self {
        self.header = Some(cells);
        self
    }

    pub fn indent(mut self, indent: usize) -> Self {
        self.indent = indent;
        self
    }

    pub fn column_gap(mut self, gap: usize) -> Self {
        self.gap = gap;
        self
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        debug_assert_eq!(cells.len(), self.aligns.len());
        self.rows.push(Row::Cells(cells));
    }

    /// A blank line between groups of related rows.
    pub fn gap(&mut self) {
        self.rows.push(Row::Gap);
    }

    /// A line built from the table's content width (excluding the indent).
    pub fn line(&mut self, render: impl Fn(usize) -> String + 'a) {
        self.rows.push(Row::Line(Box::new(render)));
    }

    pub fn render(&self) -> Vec<String> {
        let mut widths = vec![0; self.aligns.len()];
        let cell_rows = self.header.iter().chain(self.rows.iter().filter_map(|r| match r {
            Row::Cells(cells) => Some(cells),
            _ => None,
        }));
        for cells in cell_rows {
            for (w, cell) in widths.iter_mut().zip(cells) {
                *w = (*w).max(cell.plain.width());
            }
        }
        let total = widths.iter().sum::<usize>() + self.gap * widths.len().saturating_sub(1);
        let indent = " ".repeat(self.indent);
        let gap = " ".repeat(self.gap);

        let line = |cells: &[Cell]| {
            let mut out = indent.clone();
            for (i, cell) in cells.iter().enumerate() {
                if i > 0 {
                    out.push_str(&gap);
                }
                let pad = " ".repeat(widths[i] - cell.plain.width());
                match self.aligns[i] {
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

        let mut out = Vec::new();
        if let Some(header) = &self.header {
            out.push(line(header));
        }
        for row in &self.rows {
            out.push(match row {
                Row::Cells(cells) => line(cells),
                Row::Gap => String::new(),
                Row::Line(render) => format!("{indent}{}", render(total)),
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_columns() {
        let mut t = Table::new(vec![Align::Left, Align::Right])
            .header(vec![Cell::plain("A"), Cell::plain("PRICE")]);
        t.row(vec![Cell::plain("SOL"), Cell::plain("71.39")]);
        t.gap();
        t.row(vec![Cell::new("BTC", "\x1b[1mBTC\x1b[0m"), Cell::plain("95,000.00")]);
        t.line(|w| "-".repeat(w));
        assert_eq!(
            t.render(),
            vec![
                "  A         PRICE",
                "  SOL       71.39",
                "",
                "  \x1b[1mBTC\x1b[0m   95,000.00",
                "  ---------------",
            ]
        );
    }
}

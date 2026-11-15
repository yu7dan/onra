use crate::Theme;
use crate::theme::palette;

/// The sun rising over the horizon. Rows run top to bottom and are colored
/// from pale gold down to deep orange, the way a real sunrise reads.
const SUN: [&str; 3] = ["        ▁▃▅▆▆▅▃▁", "     ▗▟██████████▙▖", "    ▟██████████████▙"];
const HORIZON: &str = " ━━━━━━━━━━━━━━━━━━━━━━";
const ART_WIDTH: usize = 27;

/// The emblem with the wordmark set beside it. Used by `--version` and the
/// bare `onra` screen.
pub fn banner(theme: &Theme, version: &str) -> String {
    let rule = "─".repeat(21);
    let text = [
        String::new(),
        format!("{}  {}", theme.bold("o n r a"), theme.dim(&format!("v{version}"))),
        theme.dim(&rule),
        theme.dim("markets never sleep"),
    ];

    let mut out = String::from("\n");
    for (i, wordmark) in text.iter().enumerate() {
        let (art, styled) = match SUN.get(i) {
            Some(row) => {
                let t = i as f32 / (SUN.len() - 1) as f32;
                let color = palette::SUN_TOP.lerp(palette::SUN_BOTTOM, t);
                (*row, theme.rgb(row, color, 33))
            }
            None => (HORIZON, theme.dim(HORIZON)),
        };
        out.push_str(&styled);
        if !wordmark.is_empty() {
            let pad = ART_WIDTH.saturating_sub(art.chars().count());
            out.push_str(&" ".repeat(pad));
            out.push_str(wordmark);
        }
        out.push('\n');
    }
    out
}

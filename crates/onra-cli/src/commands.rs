//! Command implementations shared by one-shot runs and the interactive shell.
//! Each command yields an [`Outcome`]: the data for machine mode plus a
//! renderer for people. Where it's printed is the caller's business.

use std::time::{Duration, Instant};

use onra_core::Result;
use onra_ui::Theme;
use onra_venues::Venues;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::cli::ConfigAction;
use crate::config::{self, Config};
use crate::price;

type Render = Box<dyn FnOnce(&Theme) -> Vec<String>>;

pub struct Outcome {
    pub data: Value,
    render: Render,
}

impl Outcome {
    fn new(data: Value, render: impl FnOnce(&Theme) -> Vec<String> + 'static) -> Self {
        Self { data, render: Box::new(render) }
    }

    pub fn render(self, theme: &Theme) -> Vec<String> {
        (self.render)(theme)
    }
}

pub async fn price(
    venues: &Venues,
    markets: &[String],
    progress: Option<&UnboundedSender<price::Progress>>,
) -> Result<Outcome> {
    let started = Instant::now();
    let report = price::fetch(venues, markets, progress).await?;
    let elapsed = started.elapsed();
    let data = serde_json::to_value(&report).expect("report serializes");
    Ok(Outcome::new(data, move |theme| price::render(&report, theme, elapsed)))
}

pub fn config(action: ConfigAction, config: &mut Config) -> Result<Outcome> {
    match action {
        ConfigAction::Path => {
            let path = config::path()?;
            let shown = path.display().to_string();
            Ok(Outcome::new(json!({ "path": path }), move |_| vec![shown]))
        }
        ConfigAction::Show => {
            let path = config::path()?;
            let entries = config.effective();
            let values: serde_json::Map<_, _> =
                entries.iter().map(|(k, v, _)| (k.to_string(), v.clone())).collect();
            let shown_path = path.display().to_string();
            Ok(Outcome::new(json!({ "path": path, "values": values }), move |theme| {
                let mut lines =
                    vec![String::new(), format!("  {}", theme.dim(&shown_path)), String::new()];
                for (key, value, is_default) in &entries {
                    let shown = match value {
                        Value::Null => "—".to_string(),
                        Value::String(s) => s.clone(),
                        v => v.to_string(),
                    };
                    let note = if *is_default { theme.faint("  (default)") } else { String::new() };
                    lines.push(format!("  {key:<22}{}{note}", theme.strong(&shown)));
                }
                lines.push(String::new());
                lines
            }))
        }
        ConfigAction::Set { key, value } => {
            config.set(&key, &value)?;
            let path = config.save()?;
            let data = json!({ "key": key, "value": value, "path": path });
            Ok(Outcome::new(data, move |theme| {
                vec![format!("  {} {key} = {}", theme.brand("✓"), theme.strong(&value))]
            }))
        }
        ConfigAction::Unset { key } => {
            config.unset(&key)?;
            let path = config.save()?;
            Ok(Outcome::new(json!({ "key": key, "path": path }), move |theme| {
                vec![format!("  {} {key} reset to default", theme.brand("✓"))]
            }))
        }
    }
}

/// Drive `price` for a one-shot run, showing progress in place on a terminal.
pub async fn price_with_live_progress(
    venues: &Venues,
    markets: &[String],
    theme: Option<Theme>,
) -> Result<Outcome> {
    let Some(theme) = theme.filter(|_| onra_ui::motion::motion_enabled()) else {
        return price(venues, markets, None).await;
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let started = Instant::now();
    let work = price(venues, markets, Some(&tx));
    tokio::pin!(work);

    let mut loader = price::Loader::default();
    let mut live: Option<onra_ui::motion::Live> = None;
    let mut tick = tokio::time::interval(Duration::from_millis(60));
    let result = loop {
        tokio::select! {
            result = &mut work => break result,
            Some(event) = rx.recv() => loader.apply(event),
            _ = tick.tick() => {
                let elapsed = started.elapsed();
                if elapsed >= price::LOADER_DELAY {
                    let mut lines = vec![String::new()];
                    lines.extend(loader.lines(&theme, elapsed));
                    live.get_or_insert_with(onra_ui::motion::Live::new).draw(&lines);
                }
            }
        }
    };
    if let Some(live) = live {
        live.clear();
    }
    result
}

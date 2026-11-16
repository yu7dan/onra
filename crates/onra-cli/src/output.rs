//! Where output goes and what it looks like. Every command produces data
//! plus a renderer; this module picks JSON or the rendered form.

use std::ffi::OsString;
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::time::Instant;

use onra_core::{Error, ErrorCode};
use onra_proto::{Envelope, Meta};
use onra_ui::{Convention, Theme};
use serde::Serialize;

use crate::cli::Format;

pub struct Output {
    /// `None` means machine mode: JSON on stdout.
    theme: Option<Theme>,
    started: Instant,
}

impl Output {
    pub fn new(format: Format, convention: Convention, started: Instant) -> Self {
        let theme = match format {
            Format::Auto if std::io::stdout().is_terminal() => Some(Theme::detect(convention)),
            Format::Auto | Format::Json => None,
            Format::Table => Some(Theme::detect(convention)),
            Format::Plain => Some(Theme::plain(convention)),
        };
        Self { theme, started }
    }

    /// The rendering theme, or `None` in machine (JSON) mode.
    pub fn theme(&self) -> Option<&Theme> {
        self.theme.as_ref()
    }

    pub fn success<T: Serialize>(
        &self,
        data: &T,
        render: impl FnOnce(&Theme) -> String,
    ) -> ExitCode {
        match &self.theme {
            Some(theme) => print(&render(theme)),
            None => print_json(&Envelope::ok(data, Meta::new(self.started.elapsed()))),
        }
        ExitCode::SUCCESS
    }

    /// Print a command outcome: JSON in machine mode, unfolding lines otherwise.
    pub fn emit(&self, outcome: crate::commands::Outcome) -> ExitCode {
        match &self.theme {
            Some(theme) => onra_ui::motion::reveal(&outcome.render(theme)),
            None => print_json(&Envelope::ok(&outcome.data, Meta::new(self.started.elapsed()))),
        }
        ExitCode::SUCCESS
    }

    pub fn failure(&self, error: &Error) -> ExitCode {
        match &self.theme {
            Some(theme) => {
                let mut text = format!(
                    "\n  {} {}\n",
                    theme.movement("✗", std::cmp::Ordering::Less),
                    error.message
                );
                if let Some(suggest) = error.suggestion() {
                    text.push_str(&format!("    {}\n", theme.dim(&format!("→ {suggest}"))));
                }
                text.push('\n');
                let _ = std::io::stderr().write_all(text.as_bytes());
            }
            None => print_json(&Envelope::err(error, Meta::new(self.started.elapsed()))),
        }
        ExitCode::from(error.code.exit_code())
    }
}

fn print(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn print_json<T: Serialize>(value: &T) {
    let text = serde_json::to_string(value).expect("envelope serializes");
    print(&format!("{text}\n"));
}

/// Argument errors happen before we know the format, so sniff the raw args.
/// Agents must get a JSON usage error, never clap's human text.
pub fn clap_error(err: clap::Error, args: &[OsString], started: Instant) -> ExitCode {
    use clap::error::ErrorKind;
    if matches!(err.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion) {
        let _ = err.print();
        return ExitCode::SUCCESS;
    }

    let args: Vec<_> = args.iter().map(|a| a.to_string_lossy()).collect();
    let wants_json = args.iter().any(|a| a == "--json" || a == "--format=json")
        || args.windows(2).any(|w| w[0] == "--format" && w[1] == "json");
    if !wants_json && std::io::stdout().is_terminal() {
        let _ = err.print();
        return ExitCode::from(ErrorCode::Usage.exit_code());
    }

    // clap's first paragraph is the complete message ("required arguments
    // were not provided:\n  <MARKET>..."); the rest is usage text.
    let rendered = err.render().to_string();
    let message = rendered
        .split("\n\n")
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches("error: ")
        .to_string();
    let error = Error::new(ErrorCode::Usage, message)
        .with_hint(serde_json::json!({ "suggest": "onra --help" }));
    print_json(&Envelope::err(&error, Meta::new(started.elapsed())));
    ExitCode::from(ErrorCode::Usage.exit_code())
}

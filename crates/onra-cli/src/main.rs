mod cli;
mod config;
mod output;
mod price;

use std::process::ExitCode;
use std::time::Instant;

use clap::{CommandFactory, Parser};
use onra_core::{Error, ErrorCode, Result};
use onra_ui::{Convention, banner};
use onra_venues::Venues;
use serde_json::json;

use crate::cli::{Cli, Command, ConfigAction};
use crate::output::Output;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> ExitCode {
    let started = Instant::now();
    let args: Vec<_> = std::env::args_os().collect();
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(err) => return output::clap_error(err, &args, started),
    };

    let config = match config::load() {
        Ok(config) => config,
        Err(err) => {
            // `config` subcommands must keep working with a broken file so
            // the user can repair it.
            if !matches!(cli.command, Some(Command::Config { .. })) {
                return Output::new(cli.format(), Convention::West, started).failure(&err);
            }
            config::Config::default()
        }
    };
    let out = Output::new(cli.format(), config.convention(), started);

    match run(cli, config, &out) {
        Ok(code) => code,
        Err(err) => out.failure(&err),
    }
}

fn run(cli: Cli, mut config: config::Config, out: &Output) -> Result<ExitCode> {
    let Some(command) = cli.command else {
        return Ok(out.success(&json!({ "name": "onra", "version": VERSION }), |theme| {
            if cli.version {
                return banner(theme, VERSION);
            }
            let help = Cli::command().render_help().to_string();
            format!("{}\n{help}", banner(theme, VERSION))
        }));
    };

    match command {
        Command::Price { markets } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            let report = block_on(price::fetch(&venues, &markets))??;
            Ok(out.success(&report, |theme| price::render(&report, theme)))
        }
        Command::Config { action } => config_command(action, &mut config, out),
    }
}

fn config_command(
    action: ConfigAction,
    config: &mut config::Config,
    out: &Output,
) -> Result<ExitCode> {
    match action {
        ConfigAction::Path => {
            let path = config::path()?;
            Ok(out.success(&json!({ "path": path }), |_| format!("{}\n", path.display())))
        }
        ConfigAction::Show => {
            let path = config::path()?;
            let entries = config.effective();
            let values: serde_json::Map<_, _> =
                entries.iter().map(|(k, v, _)| (k.to_string(), v.clone())).collect();
            Ok(out.success(&json!({ "path": path, "values": values }), |theme| {
                let mut text = format!("\n  {}\n\n", theme.dim(&path.display().to_string()));
                for (key, value, is_default) in &entries {
                    let shown = match value {
                        serde_json::Value::Null => "—".to_string(),
                        serde_json::Value::String(s) => s.clone(),
                        v => v.to_string(),
                    };
                    let note = if *is_default { theme.dim("  (default)") } else { String::new() };
                    text.push_str(&format!("  {key:<22}{}{note}\n", theme.bold(&shown)));
                }
                text.push('\n');
                text
            }))
        }
        ConfigAction::Set { key, value } => {
            config.set(&key, &value)?;
            let path = config.save()?;
            Ok(out.success(&json!({ "key": key, "value": value, "path": path }), |theme| {
                format!("  {} {key} = {value}\n", theme.brand("✓"))
            }))
        }
        ConfigAction::Unset { key } => {
            config.unset(&key)?;
            let path = config.save()?;
            Ok(out.success(&json!({ "key": key, "path": path }), |theme| {
                format!("  {} {key} reset to default\n", theme.brand("✓"))
            }))
        }
    }
}

/// Network commands run on a single-threaded runtime: the work is I/O-bound
/// and this keeps startup cost near zero.
fn block_on<F: std::future::Future>(future: F) -> Result<F::Output> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot start runtime: {e}")))?;
    Ok(runtime.block_on(future))
}

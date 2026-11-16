mod cli;
mod commands;
mod config;
mod i18n;
mod output;
mod price;
mod shell;

use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::{CommandFactory, Parser};
use onra_core::{Error, ErrorCode, Result};
use onra_ui::{Convention, logo, motion};
use onra_venues::Venues;
use serde_json::json;

use crate::cli::{Cli, Command};
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
        if !cli.version && shell::wanted(out.theme()) {
            let theme = *out.theme().expect("checked by wanted");
            return shell::run(config, cli.proxy, theme);
        }
        let Some(theme) = out.theme() else {
            return Ok(
                out.success(&json!({ "name": "onra", "version": VERSION }), |_| String::new())
            );
        };
        println!();
        motion::play(Duration::from_millis(1500), |t| logo(t, theme.mode, 2, VERSION));
        if !cli.version {
            println!("\n{}", Cli::command().render_help());
        }
        println!();
        return Ok(ExitCode::SUCCESS);
    };

    match command {
        Command::Price { markets } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            let theme = out.theme().copied();
            let outcome = block_on(commands::price_with_live_progress(&venues, &markets, theme))??;
            Ok(out.emit(outcome))
        }
        Command::Config { action } => Ok(out.emit(commands::config(action, &mut config)?)),
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

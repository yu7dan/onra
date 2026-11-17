mod account;
mod cli;
mod commands;
mod config;
mod depth;
mod home;
mod i18n;
mod output;
mod policy;
mod price;
mod search;
mod shell;
mod signer;
mod trade;
mod watch;

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
    i18n::set(config.language());
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
        Command::Book { market, depth: levels } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            Ok(out.emit(block_on(depth::book(&venues, &market, levels))??))
        }
        Command::Funding { assets, top } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            Ok(out.emit(block_on(depth::funding(&venues, &assets, top))??))
        }
        Command::Search { query } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            Ok(out.emit(block_on(commands::search(&venues, &query.join(" ")))??))
        }
        Command::Watch { markets, interval, count } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            let interval = Duration::from_secs_f64(interval.clamp(0.2, 3600.0));
            match out.theme().copied() {
                Some(theme) => {
                    block_on(watch::run_terminal(&venues, &markets, theme, interval, count))??
                }
                None => block_on(watch::run_json(&venues, &markets, interval, count))??,
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Balance { all, hide } => {
            let parts = account::Parts { balances: true, positions: false, orders: false };
            portfolio(&config, cli.proxy, parts, all, hide, out)
        }
        Command::Positions { hide } => {
            let parts = account::Parts { balances: false, positions: true, orders: false };
            portfolio(&config, cli.proxy, parts, false, hide, out)
        }
        Command::Orders { hide } => {
            let parts = account::Parts { balances: false, positions: false, orders: true };
            portfolio(&config, cli.proxy, parts, false, hide, out)
        }
        Command::Portfolio { all, hide } => {
            let parts = account::Parts { balances: true, positions: true, orders: true };
            portfolio(&config, cli.proxy, parts, all, hide, out)
        }
        Command::Account { action } => {
            let venues = Venues::new(&config.net(cli.proxy))?;
            Ok(out.emit(block_on(account::account(action, &mut config, &venues))??))
        }
        Command::Buy(args) => trade_command(trade::Kind::Buy, args, out),
        Command::Sell(args) => trade_command(trade::Kind::Sell, args, out),
        Command::Long(args) => trade_command(trade::Kind::Long, args, out),
        Command::Short(args) => trade_command(trade::Kind::Short, args, out),
        Command::Close(args) => trade_command(trade::Kind::Close, args.into(), out),
        Command::Swap(args) => trade_command(trade::Kind::Swap, args.into(), out),
        Command::Exec { plan_id } => Ok(out.emit(block_on(trade::exec(&plan_id))??)),
        Command::Approve { plan_id } => {
            Ok(out.emit(block_on(trade::approve(&plan_id, out.theme().copied()))??))
        }
        Command::Plans => Ok(out.emit(block_on(trade::plans())??)),
        Command::Unlock { ttl } => Ok(out.emit(block_on(signer::unlock(ttl))??)),
        Command::Lock => Ok(out.emit(block_on(signer::lock())??)),
        Command::Daemon => signer::daemon::run(),
        Command::Policy { action } => Ok(out.emit(policy::policy(action)?)),
        Command::Halt => Ok(out.emit(block_on(policy::halt())??)),
        Command::Resume => Ok(out.emit(policy::resume()?)),
        Command::Audit { limit, verify } => Ok(out.emit(block_on(policy::audit(limit, verify))??)),
        Command::Config { action } => Ok(out.emit(commands::config(action, &mut config)?)),
    }
}

fn trade_command(kind: trade::Kind, args: cli::TradeArgs, out: &Output) -> Result<ExitCode> {
    Ok(out.emit(block_on(trade::trade(kind, args, out.theme().copied()))??))
}

fn portfolio(
    config: &config::Config,
    proxy: Option<String>,
    parts: account::Parts,
    all: bool,
    hide: bool,
    out: &Output,
) -> Result<ExitCode> {
    let venues = Venues::new(&config.net(proxy))?;
    Ok(out.emit(block_on(commands::portfolio(&venues, config, parts, all, hide))??))
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

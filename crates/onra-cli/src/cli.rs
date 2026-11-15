use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "onra",
    about = "The agent-native trading terminal for on-chain markets",
    disable_version_flag = true
)]
pub struct Cli {
    /// Output format. `auto` renders for humans on a terminal and emits JSON
    /// everywhere else (pipes, agents, scripts).
    #[arg(long, global = true, value_enum, default_value_t = Format::Auto)]
    pub format: Format,

    /// Shorthand for `--format json`.
    #[arg(long, global = true)]
    pub json: bool,

    /// Proxy for this invocation, e.g. http://127.0.0.1:7890. Overrides
    /// ONRA_PROXY and the config file.
    #[arg(long, global = true, value_name = "URL")]
    pub proxy: Option<String>,

    /// Print version.
    #[arg(short = 'V', long)]
    pub version: bool,

    #[command(subcommand)]
    pub command: Option<Command>,
}

impl Cli {
    pub fn format(&self) -> Format {
        if self.json { Format::Json } else { self.format }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Format {
    Auto,
    Json,
    /// Styled tables, even when not writing to a terminal.
    Table,
    /// Tables without color.
    Plain,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Price snapshot across venues.
    #[command(after_help = "\
Markets: <base>[/<quote>][-PERP][@<venue>]   venues: bn, hl, jup

Examples:
  onra price SOL                    SOL on every venue that lists it
  onra price SOL ETH BTC
  onra price ETH-PERP@hl BTC/USDT@bn")]
    Price {
        #[arg(required = true, value_name = "MARKET")]
        markets: Vec<String>,
    },

    /// Read and write configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the config file location.
    Path,
    /// Print every setting and its effective value.
    Show,
    /// Set a value, e.g. `onra config set network.proxy http://127.0.0.1:7890`.
    Set { key: String, value: String },
    /// Remove a value, restoring its default.
    Unset { key: String },
}

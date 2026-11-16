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

    /// Find an asset's markets on every venue, with verification status.
    #[command(after_help = "\
Solana is full of tokens that copy real tickers. Search shows which one is
verified (✓) and flags the rest (⚠).

Examples:
  onra search bonk
  onra search \"jupiter\"")]
    Search {
        #[arg(required = true, value_name = "QUERY")]
        query: Vec<String>,
    },

    /// Order book with depth bars.
    #[command(after_help = "\
A bare symbol shows Binance spot and Hyperliquid perps one after the other.
Jupiter routes AMM pools and has no order book.

Examples:
  onra book ETH
  onra book BTC-PERP@hl --depth 20")]
    Book {
        #[arg(value_name = "MARKET")]
        market: String,
        /// Levels per side.
        #[arg(long, default_value_t = 10)]
        depth: usize,
    },

    /// Perp funding rates, annualized so venues compare directly.
    #[command(after_help = "\
Hyperliquid funds hourly, Binance every 8 hours (some markets 4h or 1h).
Rates are shown per interval and annualized; the diff column is the gap.

Examples:
  onra funding                  the deepest Hyperliquid markets
  onra funding BTC ETH SOL")]
    Funding {
        #[arg(value_name = "ASSET")]
        assets: Vec<String>,
        /// How many markets to show without arguments.
        #[arg(long, default_value_t = 12)]
        top: usize,
    },

    /// Live prices. Press q or Esc to stop.
    #[command(after_help = "\
In JSON mode each refresh is one line (NDJSON): a \"snapshot\" event with the
full price report, then \"tick\" events. Agents should pass --count.

Examples:
  onra watch SOL ETH
  onra watch BTC-PERP@hl --interval 0.5
  onra watch SOL --json --count 5")]
    Watch {
        #[arg(required = true, value_name = "MARKET")]
        markets: Vec<String>,
        /// Seconds between refreshes.
        #[arg(long, default_value_t = 1.5, value_name = "SECS")]
        interval: f64,
        /// Stop after this many refreshes.
        #[arg(long, value_name = "N")]
        count: Option<u64>,
    },

    /// Balances across linked accounts, with a USD total.
    #[command(after_help = "Small (< $1) and unverified tokens are hidden; --all shows them.")]
    Balance {
        /// Include small and unverified balances.
        #[arg(long)]
        all: bool,
        /// Mask amounts, e.g. for screenshots.
        #[arg(long)]
        hide: bool,
    },

    /// Open perp positions.
    Positions {
        /// Mask sizes and PnL.
        #[arg(long)]
        hide: bool,
    },

    /// Resting orders.
    Orders {
        /// Mask sizes.
        #[arg(long)]
        hide: bool,
    },

    /// Balances, positions and open orders together.
    Portfolio {
        /// Include small and unverified balances.
        #[arg(long)]
        all: bool,
        /// Mask amounts, e.g. for screenshots.
        #[arg(long)]
        hide: bool,
    },

    /// Link and manage accounts.
    #[command(after_help = "\
Wallets are linked by address (public, nothing to protect):
  onra account add hl 0x...            Hyperliquid
  onra account add sol <address>       Solana (Jupiter)

Binance needs an API key. Create one with reading enabled and withdrawals
disabled; Onra refuses keys that can withdraw. The secret is typed into a
hidden prompt and kept in the system keychain:
  onra account login bn")]
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },

    /// Read and write configuration.
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum AccountAction {
    /// Show which accounts are linked.
    List,
    /// Link a wallet by address: `hl 0x...` or `sol <address>`.
    Add { venue: String, address: String },
    /// Link an exchange with an API key (prompted, never an argument).
    Login { venue: String },
    /// Unlink an account and delete its stored secret.
    Remove { venue: String },
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

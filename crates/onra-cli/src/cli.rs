use clap::{Args, Parser, Subcommand, ValueEnum};
use rust_decimal::Decimal;

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
  onra account login bn

Trading keys are separate: sealed in a passphrase-protected vault and only
ever opened by the signer (see `onra unlock`). Hyperliquid takes an API
wallet, which can trade but not withdraw:
  onra account trade hl
  onra account trade bn --testnet")]
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },

    /// Buy on Binance spot. Shows the plan and waits for Enter.
    #[command(after_help = "\
Amounts carry a unit, so nothing is ever guessed:
  0.01 BTC     a quantity of the asset
  100 USDT     spend this much of the quote
  $100         US dollars of notional
  50%  max     a share of what's available

Without --limit the order fills now within the slippage bound (0.5% by
default) or not at all; nothing is left resting. Every order is checked
against your policy first (onra policy).

Examples:
  onra buy BTC $50 --testnet
  onra buy ETH/USDC@bn 0.05 ETH --limit 1800
  onra buy SOL 100 USDT --plan          plan only, for agents: onra exec <id>")]
    Buy(TradeArgs),

    /// Sell on Binance spot. Shows the plan and waits for Enter.
    #[command(after_help = "\
Examples:
  onra sell BTC 50% --testnet
  onra sell ETH 0.1 ETH --limit 2100")]
    Sell(TradeArgs),

    /// Open or add to a long on Hyperliquid perps.
    #[command(after_help = "\
Size in the coin or in dollars: 0.05 ETH, $100. Leverage is set with --lev
(cross margin, 1x when not given) and checked against your policy.

Examples:
  onra long ETH $100 --lev 3 --testnet
  onra long BTC 0.002 BTC --limit 60000")]
    Long(TradeArgs),

    /// Open or add to a short on Hyperliquid perps.
    #[command(after_help = "\
Examples:
  onra short SOL $50 --lev 2 --testnet")]
    Short(TradeArgs),

    /// Close a Hyperliquid position, all of it or part.
    #[command(after_help = "\
Closing only ever shrinks the position, and size and daily limits never
block it: getting out is always allowed.

Examples:
  onra close ETH --testnet
  onra close BTC 50% --testnet")]
    Close(CloseArgs),

    /// Swap tokens on Solana through Jupiter.
    #[command(after_help = "\
The amount is in the token you give: 100 USDC SOL swaps 100 USDC for SOL.
Also $50 (dollars of it), 50% or max. Symbols resolve to verified tokens
only; anything else must be named by mint address.

Before signing, the signer simulates the transaction Jupiter built and
checks the wallet afterwards: only the planned amount may leave it.

Examples:
  onra swap 100 USDC SOL
  onra swap $20 SOL USDC
  onra swap 50% JUP USDC --slippage 1%")]
    Swap(SwapArgs),

    /// Carry out a plan made with --plan.
    Exec {
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
    },

    /// Approve a plan an agent made, and carry it out. Asks for the passphrase.
    Approve {
        #[arg(value_name = "PLAN_ID")]
        plan_id: String,
    },

    /// Plans the signer is holding, including those waiting for approval.
    Plans,

    /// Unlock the signer so trades can be signed. Asks for the vault passphrase.
    #[command(after_help = "\
Trading keys stay sealed in the vault until you unlock. The signer then holds
them in memory, and locks itself (forgetting them) when the time runs out.
Only a person at the keyboard can unlock; agents cannot.

Examples:
  onra unlock                  for 15 minutes
  onra unlock --for 1h")]
    Unlock {
        /// How long to stay unlocked, e.g. 30m, 2h (at most 12h).
        #[arg(long = "for", default_value = "15m", value_name = "DURATION", value_parser = humantime::parse_duration)]
        ttl: std::time::Duration,
    },

    /// Lock the signer now: keys are wiped from memory.
    Lock,

    /// The limits every trade must pass. Changing them asks for the passphrase.
    #[command(after_help = "\
The signer checks every trade against these limits before it signs, for
people and agents alike. Trades an agent starts also wait for your approval
unless agent_approval is off. The file is sealed: edit it by hand and all
trading stops until `onra policy reset`.

Examples:
  onra policy
  onra policy set max_trade_usd 250
  onra policy set venues hl,bn
  onra policy set agent_approval false")]
    Policy {
        #[command(subcommand)]
        action: Option<PolicyAction>,
    },

    /// Stop all agent trading now. Anyone can halt; resuming needs you.
    Halt,

    /// Resume agent trading after `onra halt` (asks for the passphrase).
    Resume,

    /// The sealed record of everything the signer did.
    Audit {
        /// How many recent entries to show.
        #[arg(long, default_value_t = 20)]
        limit: usize,
        /// Check that no entry was altered, removed or reordered.
        #[arg(long)]
        verify: bool,
    },

    /// Run the signer in the foreground (started for you by `onra unlock`).
    #[command(hide = true)]
    Daemon,

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
    /// Store a trading key in the vault: `bn`, `hl` (an API wallet) or `sol`.
    Trade {
        venue: String,
        /// The venue's testnet (bn, hl).
        #[arg(long)]
        testnet: bool,
        /// Delete the trading key instead.
        #[arg(long)]
        remove: bool,
    },
}

#[derive(Debug, Args)]
pub struct TradeArgs {
    /// <base>[/<quote>][@bn], e.g. BTC, ETH/USDC@bn. The quote defaults to USDT.
    #[arg(value_name = "MARKET")]
    pub market: String,
    /// How much: `0.01 BTC`, `100 USDT`, `$100`, `50%` or `max`.
    #[arg(required = true, num_args = 1..=2, value_name = "AMOUNT")]
    pub amount: Vec<String>,
    /// Rest a limit order at this price instead of filling now.
    #[arg(long, value_name = "PRICE")]
    pub limit: Option<Decimal>,
    /// How much worse than the best price a fill may be, e.g. 0.3%.
    #[arg(long, value_name = "PCT")]
    pub slippage: Option<String>,
    /// Trade on the venue's test network with play money.
    #[arg(long)]
    pub testnet: bool,
    /// Only make the plan; carry it out later with `onra exec <id>`.
    #[arg(long)]
    pub plan: bool,
    /// Skip the confirmation (a person at a terminal only; limits still apply).
    #[arg(short, long)]
    pub yes: bool,
    /// Your own order id, so a retried command cannot place a second order.
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
    /// Perp leverage (long, short), e.g. 3. Cross margin; 1x by default.
    #[arg(long, value_name = "N")]
    pub lev: Option<Decimal>,
    /// Set by `onra swap`.
    #[arg(skip)]
    pub swap_to: Option<String>,
}

#[derive(Debug, Args)]
pub struct SwapArgs {
    /// How much to give: `100`, `$50`, `50%` or `max`.
    #[arg(value_name = "AMOUNT")]
    pub amount: String,
    /// The token you give: a symbol (verified tokens only) or a mint.
    #[arg(value_name = "FROM")]
    pub from: String,
    /// The token you get.
    #[arg(value_name = "TO")]
    pub to: String,
    /// How much less than quoted the swap may deliver, e.g. 0.3%.
    #[arg(long, value_name = "PCT")]
    pub slippage: Option<String>,
    #[arg(long)]
    pub plan: bool,
    #[arg(short, long)]
    pub yes: bool,
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
}

impl From<SwapArgs> for TradeArgs {
    fn from(s: SwapArgs) -> Self {
        // A bare number is in the token given; $, % and max speak for themselves.
        let amount = if s.amount.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
            vec![s.amount, s.from.clone()]
        } else {
            vec![s.amount]
        };
        TradeArgs {
            market: s.from,
            amount,
            limit: None,
            slippage: s.slippage,
            testnet: false,
            plan: s.plan,
            yes: s.yes,
            client_id: s.client_id,
            lev: None,
            swap_to: Some(s.to),
        }
    }
}

#[derive(Debug, Args)]
pub struct CloseArgs {
    /// The perp, e.g. ETH or ETH-PERP@hl.
    #[arg(value_name = "MARKET")]
    pub market: String,
    /// How much of the position: `50%`, `0.1 ETH`, `$100`. All of it by default.
    #[arg(num_args = 0..=2, value_name = "AMOUNT")]
    pub amount: Vec<String>,
    /// Rest a limit order at this price instead of closing now.
    #[arg(long, value_name = "PRICE")]
    pub limit: Option<Decimal>,
    /// How much worse than the best price a fill may be, e.g. 0.3%.
    #[arg(long, value_name = "PCT")]
    pub slippage: Option<String>,
    #[arg(long)]
    pub testnet: bool,
    #[arg(long)]
    pub plan: bool,
    #[arg(short, long)]
    pub yes: bool,
    #[arg(long, value_name = "ID")]
    pub client_id: Option<String>,
}

impl From<CloseArgs> for TradeArgs {
    fn from(c: CloseArgs) -> Self {
        let amount = if c.amount.is_empty() { vec!["100%".into()] } else { c.amount };
        TradeArgs {
            market: c.market,
            amount,
            limit: c.limit,
            slippage: c.slippage,
            testnet: c.testnet,
            plan: c.plan,
            yes: c.yes,
            client_id: c.client_id,
            lev: None,
            swap_to: None,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum PolicyAction {
    /// Show the limits and today's usage.
    Show,
    /// Change one limit, e.g. `max_trade_usd 250`.
    Set { key: String, value: String },
    /// Back to the defaults ($100 a trade, $500 a day, 3x, 1%).
    Reset,
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

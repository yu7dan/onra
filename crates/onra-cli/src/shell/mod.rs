//! The interactive shell: what `onra` opens on a terminal.
//!
//! Output scrolls up like any terminal session while a panel stays anchored
//! at the bottom: venue status, a menu or guided prompt, the input box and
//! key hints. Everything typed here runs through the same commands as the
//! one-shot CLI, so what people learn here works in scripts and for agents.

mod complete;
mod editor;
mod screen;

use std::future::Future;
use std::pin::Pin;
use std::process::ExitCode;
use std::rc::Rc;
use std::time::{Duration, Instant};

use clap::Parser;
use crossterm::event::{Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use onra_core::{Error, ErrorCode, Result, VenueId};
use onra_ui::{Theme, logo, motion, text};
use onra_venues::Venues;
use tokio::sync::mpsc::{self, UnboundedSender};

use self::complete::{COMMANDS, POPULAR};
use self::editor::Editor;
use self::screen::{Frame, Screen};
use crate::cli::{Cli, Command, ConfigAction};
use crate::commands::{self, Outcome};
use crate::config::Config;
use crate::i18n::{self, Lang, Strings};
use crate::price::{self, Loader, Progress};
use crate::watch::{self, Update, Watch};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const PING_EVERY: Duration = Duration::from_secs(60);
/// Menu entries that work today; the rest show as "coming soon".
const MENU_READY: [bool; 7] = [true, true, true, true, false, true, true];

/// What a finished background task produced.
enum Done {
    Outcome(Result<Outcome>),
    Watch(Result<price::Report>),
}

type Task = Pin<Box<dyn Future<Output = Done>>>;
type Polling = Pin<Box<dyn Future<Output = Vec<Update>>>>;
type TotalTask = Pin<Box<dyn Future<Output = Result<crate::account::Portfolio>>>>;

/// The home screen's account total, fetched in the background.
enum Total {
    /// No accounts linked: the line isn't shown.
    Unlinked,
    Loading,
    Known(Option<rust_decimal::Decimal>),
    Failed,
}

/// What the asset picker leads to.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Purpose {
    Price,
    Watch,
}
type Ping = Pin<Box<dyn Future<Output = (VenueId, Result<Duration>)>>>;

enum Mode {
    Menu,
    Input,
    PickAsset,
    PickVenue { asset: String },
    Busy { started: Instant, loader: Loader, json: bool },
    Watching { watch: Watch, interval: Duration, next_poll: Instant },
}

enum Health {
    Checking,
    Up(Duration),
    Down,
}

struct Shell {
    theme: Theme,
    s: &'static Strings,
    config: Config,
    proxy: Option<String>,
    venues: Rc<Venues>,
    mode: Mode,
    menu: usize,
    pick: usize,
    purpose: Purpose,
    watch_interval: Duration,
    editor: Editor,
    /// Completion candidates being cycled with Tab, and the current one.
    tab: Option<(Vec<String>, usize)>,
    /// A one-line notice above the input, cleared by the next key.
    note: Option<String>,
    health: Vec<(VenueId, Health)>,
    last_ping: Instant,
    accounts: Rc<onra_venues::Accounts>,
    total: Total,
    /// The total is masked until asked for (screenshots, screen sharing).
    show_total: bool,
    refresh_total: bool,
    quit: bool,
}

/// Restores the terminal however the shell exits, including on panic.
struct RawMode;

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        print!("\x1b[?25h");
    }
}

pub fn run(config: Config, proxy: Option<String>, theme: Theme) -> Result<ExitCode> {
    let venues = Rc::new(Venues::new(&config.net(proxy.clone()))?);
    println!();
    motion::play(Duration::from_millis(1500), |t| logo(t, theme.mode, 2, VERSION));

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot start runtime: {e}")))?;
    crossterm::terminal::enable_raw_mode()
        .map_err(|e| Error::new(ErrorCode::Internal, format!("cannot enter raw mode: {e}")))?;
    let _raw = RawMode;

    // Credentials load before raw mode, so a keystore passphrase prompt (only
    // needed without a system keychain) works normally.
    let accounts = Rc::new(load_accounts(&config));
    let mut shell = Shell::new(config, proxy, theme, venues, i18n::lang(), Editor::with_history());
    shell.set_accounts(accounts);
    runtime.block_on(shell.event_loop());
    Ok(ExitCode::SUCCESS)
}

impl Shell {
    fn new(
        config: Config,
        proxy: Option<String>,
        theme: Theme,
        venues: Rc<Venues>,
        lang: Lang,
        editor: Editor,
    ) -> Self {
        Self {
            theme,
            s: i18n::strings(lang),
            config,
            proxy,
            venues,
            mode: Mode::Menu,
            menu: 0,
            pick: 0,
            purpose: Purpose::Price,
            watch_interval: Duration::from_millis(1500),
            editor,
            tab: None,
            note: None,
            health: VenueId::ALL.into_iter().map(|v| (v, Health::Checking)).collect(),
            last_ping: Instant::now(),
            accounts: Rc::new(onra_venues::Accounts::default()),
            total: Total::Unlinked,
            show_total: false,
            refresh_total: false,
            quit: false,
        }
    }

    fn set_accounts(&mut self, accounts: Rc<onra_venues::Accounts>) {
        let linked = accounts.binance.is_some()
            || accounts.hyperliquid.is_some()
            || accounts.solana.is_some();
        self.accounts = accounts;
        self.total = if linked { Total::Loading } else { Total::Unlinked };
        self.refresh_total = linked;
    }

    fn fetch_total(&mut self) -> Option<TotalTask> {
        if matches!(self.total, Total::Unlinked) {
            return None;
        }
        self.refresh_total = false;
        let venues = self.venues.clone();
        let accounts = self.accounts.clone();
        let parts = crate::account::Parts { balances: true, positions: false, orders: false };
        Some(Box::pin(async move { crate::account::fetch(&venues, &accounts, parts).await }))
    }

    fn total_line(&self) -> Option<String> {
        let t = &self.theme;
        let value = match &self.total {
            Total::Unlinked => return None,
            Total::Loading => t.faint("…"),
            Total::Failed => t.faint("—"),
            Total::Known(None) => t.faint("—"),
            Total::Known(Some(v)) if self.show_total => {
                t.strong(&format!("${}", onra_ui::format::price(*v, 2)))
            }
            Total::Known(Some(_)) => t.strong("••••••"),
        };
        let hint = if self.show_total { self.s.hide_total } else { self.s.show_total };
        Some(format!("  {}   {value}   {}", t.faint(self.s.total), t.faint(hint)))
    }

    async fn event_loop(mut self) {
        let mut screen = Screen::default();
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(80));
        let (tx, mut rx) = mpsc::unbounded_channel::<Progress>();
        let mut task: Option<Task> = None;
        let mut polling: Option<Polling> = None;
        let mut pings = self.ping_all();
        let mut total_task: Option<TotalTask> = self.fetch_total();
        screen.draw(&self.frame());

        loop {
            tokio::select! {
                event = events.next() => {
                    match event {
                        Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                            if let Some(new) = self.on_key(key, &mut screen, &tx) {
                                task = Some(new);
                            }
                            if matches!(self.mode, Mode::Input | Mode::Menu) && task.is_some() {
                                // A key cancelled the running task.
                                task = None;
                            }
                            if !matches!(self.mode, Mode::Watching { .. }) {
                                polling = None;
                            }
                        }
                        Some(Ok(Event::Resize(..))) => screen.reset(),
                        Some(Ok(_)) => {}
                        Some(Err(_)) | None => break,
                    }
                    if self.quit {
                        break;
                    }
                    if self.refresh_total && total_task.is_none() {
                        total_task = self.fetch_total();
                    }
                    screen.draw(&self.frame());
                }
                result = async { total_task.as_mut().expect("guarded by condition").await }, if total_task.is_some() => {
                    total_task = None;
                    self.total = match result {
                        Ok(p) => Total::Known(p.total_usd),
                        Err(_) => Total::Failed,
                    };
                    screen.draw(&self.frame());
                }
                Some(event) = rx.recv() => {
                    if let Mode::Busy { loader, .. } = &mut self.mode {
                        loader.apply(event);
                    }
                }
                result = async { task.as_mut().expect("guarded by condition").await }, if task.is_some() => {
                    task = None;
                    self.finish(result, &mut screen);
                }
                updates = async { polling.as_mut().expect("guarded by condition").await }, if polling.is_some() => {
                    polling = None;
                    if let Mode::Watching { watch, .. } = &mut self.mode {
                        watch.apply(updates);
                    }
                }
                Some((venue, result)) = pings.next(), if !pings.is_empty() => {
                    self.set_health(venue, result.map_or(Health::Down, Health::Up));
                    screen.draw(&self.frame());
                }
                _ = tick.tick() => {
                    let checking = self.health.iter().any(|(_, h)| matches!(h, Health::Checking));
                    if let Mode::Watching { watch, interval, next_poll } = &mut self.mode
                        && polling.is_none()
                        && Instant::now() >= *next_poll
                    {
                        let venues = self.venues.clone();
                        let markets = watch.markets_due(*interval);
                        if !markets.is_empty() {
                            polling = Some(Box::pin(async move { watch::poll(&venues, markets).await }));
                        }
                        *next_poll = Instant::now() + *interval;
                    }
                    if matches!(self.mode, Mode::Busy { .. } | Mode::Watching { .. }) || checking {
                        screen.draw(&self.frame());
                    }
                    if pings.is_empty() && self.last_ping.elapsed() >= PING_EVERY {
                        pings = self.ping_all();
                        if total_task.is_none() {
                            total_task = self.fetch_total();
                        }
                    }
                }
            }
        }

        screen.erase();
        println!("  {}\r", self.theme.faint(self.s.bye));
    }

    fn ping_all(&mut self) -> FuturesUnordered<Ping> {
        self.last_ping = Instant::now();
        VenueId::ALL
            .into_iter()
            .map(|venue| {
                let venues = self.venues.clone();
                Box::pin(async move {
                    let started = Instant::now();
                    (venue, venues.ping(venue).await.map(|_| started.elapsed()))
                }) as Ping
            })
            .collect()
    }

    fn set_health(&mut self, venue: VenueId, health: Health) {
        if let Some((_, h)) = self.health.iter_mut().find(|(v, _)| *v == venue) {
            *h = health;
        }
    }

    // -----------------------------------------------------------------------
    // Keys

    fn on_key(
        &mut self,
        key: KeyEvent,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.note = None;

        if ctrl && key.code == KeyCode::Char('o') {
            self.show_total = !self.show_total;
            return None;
        }

        if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('d')) {
            match self.mode {
                Mode::Busy { .. } => {
                    self.mode = Mode::Input;
                    self.note = Some(self.theme.faint(self.s.cancelled));
                }
                Mode::Watching { .. } => self.stop_watch(screen),
                _ if !self.editor.is_empty() => self.editor.clear(),
                _ => self.quit = true,
            }
            return None;
        }

        match self.mode {
            Mode::Busy { .. } => None,
            Mode::Menu => self.on_menu_key(key, screen, tx),
            Mode::Input => self.on_input_key(key, ctrl, screen, tx),
            Mode::PickAsset => self.on_pick_asset_key(key, ctrl, screen, tx),
            Mode::Watching { .. } => {
                if matches!(key.code, KeyCode::Esc | KeyCode::Char('q' | 'Q')) {
                    self.stop_watch(screen);
                }
                None
            }
            Mode::PickVenue { .. } => self.on_pick_venue_key(key, screen, tx),
        }
    }

    fn on_menu_key(
        &mut self,
        key: KeyEvent,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        match key.code {
            KeyCode::Up => self.menu = (self.menu + MENU_READY.len() - 1) % MENU_READY.len(),
            KeyCode::Down => self.menu = (self.menu + 1) % MENU_READY.len(),
            KeyCode::Enter => return self.activate_menu(screen, tx),
            KeyCode::Tab => self.mode = Mode::Input,
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.mode = Mode::Input;
                self.editor.insert(ch);
            }
            _ => {}
        }
        None
    }

    fn activate_menu(
        &mut self,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        if !MENU_READY[self.menu] {
            let (label, _) = self.s.menu[self.menu];
            self.note = Some(self.theme.faint(&format!("{label} · {}", self.s.soon)));
            return None;
        }
        match self.menu {
            0 | 1 => {
                self.editor.clear();
                self.pick = 0;
                self.purpose = if self.menu == 0 { Purpose::Price } else { Purpose::Watch };
                self.mode = Mode::PickAsset;
                None
            }
            3 => self.run_line("portfolio", screen, tx),
            2 => {
                self.editor.set("search ");
                self.mode = Mode::Input;
                None
            }
            5 => self.run_line("config show", screen, tx),
            6 => {
                self.quit = true;
                None
            }
            _ => None,
        }
    }

    fn on_input_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        if key.code != KeyCode::Tab {
            self.tab = None;
        }
        match key.code {
            KeyCode::Enter => {
                let line = self.editor.text();
                self.editor.commit(&line);
                self.editor.clear();
                return self.run_line(line.trim(), screen, tx);
            }
            KeyCode::Esc => {
                self.editor.clear();
                self.mode = Mode::Menu;
            }
            KeyCode::Tab => self.cycle_completion(),
            KeyCode::Right if self.editor.at_end() => match self.suggestion() {
                Some(s) => self.editor.set(&s),
                None => self.editor.right(),
            },
            KeyCode::Up => self.editor.history_prev(),
            KeyCode::Down => self.editor.history_next(),
            KeyCode::Left => self.editor.left(),
            KeyCode::Right => self.editor.right(),
            KeyCode::Home => self.editor.home(),
            KeyCode::End => self.editor.end(),
            KeyCode::Backspace => self.editor.backspace(),
            KeyCode::Delete => self.editor.delete(),
            KeyCode::Char('u') if ctrl => self.editor.clear(),
            KeyCode::Char('w') if ctrl => self.editor.delete_word(),
            KeyCode::Char(ch) if !ctrl => self.editor.insert(ch),
            _ => {}
        }
        None
    }

    fn cycle_completion(&mut self) {
        match &mut self.tab {
            Some((options, i)) => {
                *i = (*i + 1) % options.len();
                self.editor.set(&options[*i]);
            }
            None => {
                let options = complete::complete(&self.editor.text());
                match options.len() {
                    0 => {}
                    1 => self.editor.set(&format!("{} ", options[0])),
                    _ => {
                        self.editor.set(&options[0]);
                        self.tab = Some((options, 0));
                    }
                }
            }
        }
    }

    /// The full line suggested inline: history first, then a unique completion.
    fn suggestion(&self) -> Option<String> {
        if !matches!(self.mode, Mode::Input) || !self.editor.at_end() || self.editor.is_empty() {
            return None;
        }
        if let Some(h) = self.editor.history_match() {
            return Some(h.to_string());
        }
        let options = complete::complete(&self.editor.text());
        (options.len() == 1).then(|| options[0].clone())
    }

    fn asset_options(&self) -> Vec<&'static str> {
        let typed = self.editor.text().to_ascii_uppercase();
        POPULAR.iter().copied().filter(|s| s.starts_with(typed.trim())).take(6).collect()
    }

    fn on_pick_asset_key(
        &mut self,
        key: KeyEvent,
        ctrl: bool,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        let options = self.asset_options();
        match key.code {
            KeyCode::Esc => {
                self.editor.clear();
                self.mode = Mode::Menu;
            }
            KeyCode::Up if !options.is_empty() => {
                self.pick = (self.pick + options.len() - 1) % options.len()
            }
            KeyCode::Down if !options.is_empty() => self.pick = (self.pick + 1) % options.len(),
            KeyCode::Enter => {
                let typed = self.editor.text().trim().to_ascii_uppercase();
                let asset = match options.get(self.pick) {
                    Some(s) => s.to_string(),
                    None if !typed.is_empty() => typed,
                    None => return None,
                };
                self.editor.clear();
                self.pick = 0;
                if self.purpose == Purpose::Watch {
                    let line = format!("watch {asset}");
                    self.editor.commit(&line);
                    return self.run_line(&line, screen, tx);
                }
                self.mode = Mode::PickVenue { asset };
            }
            KeyCode::Backspace => {
                self.editor.backspace();
                self.pick = 0;
            }
            KeyCode::Char(ch) if !ctrl && !ch.is_whitespace() => {
                self.editor.insert(ch);
                self.pick = 0;
            }
            _ => {}
        }
        None
    }

    fn on_pick_venue_key(
        &mut self,
        key: KeyEvent,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        let count = VenueId::ALL.len() + 1;
        match key.code {
            KeyCode::Esc => {
                self.pick = 0;
                self.mode = Mode::PickAsset;
            }
            KeyCode::Up => self.pick = (self.pick + count - 1) % count,
            KeyCode::Down => self.pick = (self.pick + 1) % count,
            KeyCode::Enter => {
                let Mode::PickVenue { asset } = &self.mode else { return None };
                let line = match self.pick {
                    0 => format!("price {asset}"),
                    i => format!("price {asset}@{}", VenueId::ALL[i - 1].alias()),
                };
                self.pick = 0;
                // Guided runs land in history too: the command is the lesson.
                self.editor.commit(&line);
                return self.run_line(&line, screen, tx);
            }
            _ => {}
        }
        None
    }

    // -----------------------------------------------------------------------
    // Running commands

    fn print(&self, screen: &mut Screen, lines: &[String]) {
        screen.print_above(lines, &self.frame());
    }

    fn echo(&self, screen: &mut Screen, line: &str) {
        let echo = format!("  {} {}", self.theme.brand("❯"), self.theme.strong(line));
        self.print(screen, &[String::new(), echo]);
    }

    fn run_line(
        &mut self,
        line: &str,
        screen: &mut Screen,
        tx: &UnboundedSender<Progress>,
    ) -> Option<Task> {
        self.mode = Mode::Input;
        if line.is_empty() {
            return None;
        }
        let words = match shell_words::split(line) {
            Ok(words) => words,
            Err(e) => {
                self.echo(screen, line);
                self.print_error(screen, &Error::new(ErrorCode::Usage, e.to_string()));
                return None;
            }
        };
        let words: Vec<String> =
            if words.first().is_some_and(|w| w == "onra") { words[1..].to_vec() } else { words };
        let first = words.first().map(String::as_str)?;

        match first {
            "exit" | "quit" => {
                self.quit = true;
                return None;
            }
            "clear" => {
                screen.reset();
                return None;
            }
            "menu" | "home" => {
                self.mode = Mode::Menu;
                return None;
            }
            "help" => {
                self.echo(screen, line);
                self.print(screen, &self.help_lines());
                return None;
            }
            "price" | "watch" if words.len() == 1 => {
                self.editor.clear();
                self.pick = 0;
                self.purpose = if first == "price" { Purpose::Price } else { Purpose::Watch };
                self.mode = Mode::PickAsset;
                return None;
            }
            _ => {}
        }

        self.echo(screen, line);
        if !COMMANDS.contains(&first) && !first.starts_with('-') {
            self.print(screen, &self.natural_language_lines(&words));
            return None;
        }

        let cli = match Cli::try_parse_from(std::iter::once("onra".to_string()).chain(words)) {
            Ok(cli) => cli,
            Err(e) => {
                let rendered = e.render().to_string();
                let message = rendered
                    .split("\n\n")
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                self.print_error(
                    screen,
                    &Error::new(
                        ErrorCode::Usage,
                        message.trim_start_matches("error: ").to_string(),
                    ),
                );
                return None;
            }
        };
        let json = cli.json || cli.format == crate::cli::Format::Json;
        match cli.command {
            Some(Command::Price { markets }) => {
                let venues = self.venues.clone();
                let tx = tx.clone();
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move {
                    Done::Outcome(commands::price(&venues, &markets, Some(&tx)).await)
                }))
            }
            Some(Command::Book { market, depth }) => {
                let venues = self.venues.clone();
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move {
                    Done::Outcome(crate::depth::book(&venues, &market, depth).await)
                }))
            }
            Some(Command::Funding { assets, top }) => {
                let venues = self.venues.clone();
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move {
                    Done::Outcome(crate::depth::funding(&venues, &assets, top).await)
                }))
            }
            Some(Command::Search { query }) => {
                let venues = self.venues.clone();
                let query = query.join(" ");
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(
                    async move { Done::Outcome(commands::search(&venues, &query).await) },
                ))
            }
            Some(
                cmd @ (Command::Balance { .. }
                | Command::Positions { .. }
                | Command::Orders { .. }
                | Command::Portfolio { .. }),
            ) => {
                let (parts, all, hide) = match cmd {
                    Command::Balance { all, hide } => (
                        crate::account::Parts { balances: true, positions: false, orders: false },
                        all,
                        hide,
                    ),
                    Command::Positions { hide } => (
                        crate::account::Parts { balances: false, positions: true, orders: false },
                        false,
                        hide,
                    ),
                    Command::Orders { hide } => (
                        crate::account::Parts { balances: false, positions: false, orders: true },
                        false,
                        hide,
                    ),
                    Command::Portfolio { all, hide } => (
                        crate::account::Parts { balances: true, positions: true, orders: true },
                        all,
                        hide,
                    ),
                    _ => unreachable!("matched above"),
                };
                let venues = self.venues.clone();
                // Loading accounts may prompt for a keystore passphrase, which
                // can't happen inside the shell's raw mode; the keychain case
                // never prompts.
                let config = self.config.clone();
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move {
                    Done::Outcome(commands::portfolio(&venues, &config, parts, all, hide).await)
                }))
            }
            Some(Command::Account { action: crate::cli::AccountAction::Login { .. } }) => {
                self.print_error(
                    screen,
                    &Error::new(
                        ErrorCode::Usage,
                        "linking an exchange key needs a regular terminal",
                    )
                    .with_hint(serde_json::json!({ "suggest": "onra account login bn" })),
                );
                None
            }
            // Hidden prompts can't run inside the shell's raw mode.
            Some(Command::Account {
                action: crate::cli::AccountAction::Trade { venue, testnet, remove: false },
            }) => {
                let flag = if testnet { " --testnet" } else { "" };
                self.print_error(
                    screen,
                    &Error::new(ErrorCode::Usage, "adding a trading key needs a regular terminal")
                        .with_hint(serde_json::json!({ "suggest": format!("onra account trade {venue}{flag}") })),
                );
                None
            }
            Some(Command::Unlock { .. }) => {
                self.print_error(
                    screen,
                    &Error::new(ErrorCode::Usage, "unlocking needs a regular terminal for now")
                        .with_hint(serde_json::json!({ "suggest": "onra unlock" })),
                );
                None
            }
            Some(Command::Daemon) => None,
            // Trading confirms on a card with its own key handling; the shell
            // gets that in a later step.
            Some(
                Command::Buy(_)
                | Command::Sell(_)
                | Command::Long(_)
                | Command::Short(_)
                | Command::Close(_)
                | Command::Swap(_)
                | Command::Exec { .. }
                | Command::Approve { .. },
            ) => {
                self.print_error(
                    screen,
                    &Error::new(
                        ErrorCode::Usage,
                        "trading from the shell is coming; use a regular terminal for now",
                    )
                    .with_hint(serde_json::json!({ "suggest": "onra buy BTC $20 --testnet" })),
                );
                None
            }
            Some(Command::Plans) => {
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move { Done::Outcome(crate::trade::plans().await) }))
            }
            // Reading the policy needs nothing; changing it needs a prompt.
            Some(Command::Policy { action: None | Some(crate::cli::PolicyAction::Show) }) => {
                match crate::policy::policy(None) {
                    Ok(outcome) => {
                        let lines = self.outcome_lines(outcome, json);
                        self.print(screen, &lines);
                    }
                    Err(e) => self.print_error(screen, &e),
                }
                None
            }
            Some(Command::Policy { .. } | Command::Resume) => {
                self.print_error(
                    screen,
                    &Error::new(
                        ErrorCode::Usage,
                        "this asks for the passphrase; use a regular terminal",
                    )
                    .with_hint(serde_json::json!({ "suggest": "onra policy set <key> <value>" })),
                );
                None
            }
            Some(Command::Halt) => {
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move { Done::Outcome(crate::policy::halt().await) }))
            }
            Some(Command::Audit { limit, verify: false }) => {
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(
                    async move { Done::Outcome(crate::policy::audit(limit, false).await) },
                ))
            }
            Some(Command::Audit { verify: true, .. }) => {
                self.print_error(
                    screen,
                    &Error::new(
                        ErrorCode::Usage,
                        "verifying may ask for the passphrase; use a regular terminal",
                    )
                    .with_hint(serde_json::json!({ "suggest": "onra audit --verify" })),
                );
                None
            }
            Some(Command::Lock) => {
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move { Done::Outcome(crate::signer::lock().await) }))
            }
            // Listing talks to the signer, so it runs on the event loop.
            Some(Command::Account { action: action @ crate::cli::AccountAction::List }) => {
                let venues = self.venues.clone();
                let mut config = self.config.clone();
                self.mode = Mode::Busy { started: Instant::now(), loader: Loader::default(), json };
                Some(Box::pin(async move {
                    Done::Outcome(crate::account::account(action, &mut config, &venues).await)
                }))
            }
            Some(Command::Account { action }) => {
                let venues = self.venues.clone();
                let mut config = std::mem::take(&mut self.config);
                let result = futures::executor::block_on(crate::account::account(
                    action,
                    &mut config,
                    &venues,
                ));
                self.config = config;
                self.set_accounts(Rc::new(load_accounts(&self.config)));
                match result {
                    Ok(outcome) => {
                        let lines = self.outcome_lines(outcome, json);
                        self.print(screen, &lines);
                    }
                    Err(e) => self.print_error(screen, &e),
                }
                None
            }
            Some(Command::Watch { markets, interval, .. }) => {
                let venues = self.venues.clone();
                let tx = tx.clone();
                self.watch_interval = Duration::from_secs_f64(interval.clamp(0.2, 3600.0));
                self.mode =
                    Mode::Busy { started: Instant::now(), loader: Loader::default(), json: false };
                Some(Box::pin(async move {
                    Done::Watch(price::fetch(&venues, &markets, Some(&tx)).await)
                }))
            }
            Some(Command::Config { action }) => {
                let rewires = matches!(&action, ConfigAction::Set { key, .. } | ConfigAction::Unset { key } if key.starts_with("network."));
                match commands::config(action, &mut self.config) {
                    Ok(outcome) => {
                        self.theme.convention = self.config.convention();
                        let lines = self.outcome_lines(outcome, json);
                        self.print(screen, &lines);
                    }
                    Err(e) => self.print_error(screen, &e),
                }
                if rewires {
                    match Venues::new(&self.config.net(self.proxy.clone())) {
                        Ok(v) => self.venues = Rc::new(v),
                        Err(e) => self.print_error(screen, &e),
                    }
                    self.last_ping = Instant::now() - PING_EVERY;
                }
                None
            }
            None => {
                self.print(screen, &self.help_lines());
                None
            }
        }
    }

    fn finish(&mut self, done: Done, screen: &mut Screen) {
        let json = matches!(self.mode, Mode::Busy { json: true, .. });
        self.mode = Mode::Input;
        match done {
            Done::Outcome(Ok(outcome)) => {
                let lines = self.outcome_lines(outcome, json);
                self.print(screen, &lines);
            }
            Done::Watch(Ok(report)) => {
                let interval = self.watch_interval;
                let watch = Watch::from_report(&report);
                self.mode =
                    Mode::Watching { watch, interval, next_poll: Instant::now() + interval };
            }
            Done::Outcome(Err(e)) | Done::Watch(Err(e)) => self.print_error(screen, &e),
        }
    }

    /// End a watch, keeping its last frame in the scrollback.
    fn stop_watch(&mut self, screen: &mut Screen) {
        let Mode::Watching { watch, interval, .. } = std::mem::replace(&mut self.mode, Mode::Input)
        else {
            return;
        };
        let mut lines = vec![String::new()];
        lines.extend(watch.render(&self.theme, interval, false));
        lines.push(String::new());
        self.print(screen, &lines);
    }

    fn outcome_lines(&self, outcome: Outcome, json: bool) -> Vec<String> {
        if json {
            let pretty = serde_json::to_string_pretty(&outcome.data).unwrap_or_default();
            return pretty.lines().map(|l| format!("  {}", self.theme.dim(l))).collect();
        }
        outcome.render(&self.theme)
    }

    fn print_error(&self, screen: &mut Screen, error: &Error) {
        let mut lines =
            vec![String::new(), format!("  {} {}", self.theme.warn("✗"), i18n::error_text(error))];
        if let Some(s) = error.suggestion() {
            lines.push(format!("    {}", self.theme.dim(&format!("→ {s}"))));
        }
        lines.push(String::new());
        self.print(screen, &lines);
    }

    fn natural_language_lines(&self, words: &[String]) -> Vec<String> {
        let t = &self.theme;
        let symbol =
            words.iter().map(|w| w.to_ascii_uppercase()).find(|w| POPULAR.contains(&w.as_str()));
        let guess = symbol.map_or_else(|| "help".to_string(), |s| format!("price {s}"));
        vec![
            String::new(),
            format!("  {} {}", t.brand("·"), t.dim(self.s.nl_soon)),
            format!("    {} {}", t.faint(&format!("{}:", self.s.nl_try)), t.strong(&guess)),
            String::new(),
        ]
    }

    fn help_lines(&self) -> Vec<String> {
        let t = &self.theme;
        let width = self.s.help.iter().map(|(c, _)| text::width(c)).max().unwrap_or(0);
        let mut lines = vec![String::new(), format!("  {}", t.strong(self.s.help_title))];
        for (cmd, desc) in self.s.help.iter() {
            lines.push(format!("    {}  {}", t.brand(&text::pad(cmd, width)), t.dim(desc)));
        }
        lines.push(String::new());
        lines
    }

    // -----------------------------------------------------------------------
    // The panel

    fn frame(&self) -> Frame {
        let t = &self.theme;
        let term = crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80);
        let inner = term.saturating_sub(6).clamp(36, 96);

        let mut lines = vec![String::new(), self.status_line(inner + 2)];
        if let Some(total) = self.total_line() {
            lines.push(total);
        }
        lines.push(String::new());
        let body = self.body();
        if !body.is_empty() {
            lines.extend(body);
            lines.push(String::new());
        }
        if let Some(note) = &self.note {
            lines.push(format!("  {note}"));
            lines.push(String::new());
        }

        let (input, col) = self.input_line();
        // The box brightens while it holds the focus.
        let edge = match self.mode {
            Mode::Input | Mode::PickAsset => onra_ui::palette::MUTED,
            _ => onra_ui::palette::FAINT,
        };
        let border = |s: &str| t.rgb(s, edge);
        lines.push(format!("  {}", border(&format!("╭{}╮", "─".repeat(inner)))));
        let input_row = lines.len();
        lines.push(format!(
            "  {}{}{}",
            border("│"),
            text::pad(&text::truncate(&input, inner), inner),
            border("│")
        ));
        lines.push(format!("  {}", border(&format!("╰{}╯", "─".repeat(inner)))));
        lines.push(format!("    {}", t.faint(self.hints())));

        let cursor = match self.mode {
            Mode::Busy { .. } | Mode::PickVenue { .. } | Mode::Watching { .. } => None,
            _ => Some((input_row, 3 + col.min(inner.saturating_sub(1)))),
        };
        Frame { lines, cursor }
    }

    fn status_line(&self, width: usize) -> String {
        let t = &self.theme;
        let mut left = String::from("  ");
        for (i, (venue, health)) in self.health.iter().enumerate() {
            if i > 0 {
                left.push_str("   ");
            }
            let (dot, detail) = match health {
                Health::Checking => (t.faint("○"), t.faint("…")),
                Health::Up(d) => (
                    t.rgb("●", price::venue_color(*venue)),
                    t.faint(&format!("{}ms", d.as_millis())),
                ),
                Health::Down => (t.warn("○"), t.faint(self.s.offline)),
            };
            left.push_str(&format!("{dot} {} {detail}", t.dim(venue.name())));
        }
        let right = format!("{} {}", t.brand("☀"), t.faint(&format!("onra v{VERSION}")));
        let pad = (width + 2).saturating_sub(text::width(&left) + text::width(&right)).max(2);
        format!("{left}{}{right}", " ".repeat(pad))
    }

    fn body(&self) -> Vec<String> {
        let t = &self.theme;
        match &self.mode {
            Mode::Menu => {
                let mut lines = vec![format!("  {}", t.strong(self.s.ask))];
                let width = self.s.menu.iter().map(|(l, _)| text::width(l)).max().unwrap_or(0);
                for (i, (label, desc)) in self.s.menu.iter().enumerate() {
                    let label_padded = text::pad(label, width);
                    let line = if !MENU_READY[i] {
                        format!(
                            "    {}   {}",
                            t.faint(&label_padded),
                            t.faint(&format!("{desc} · {}", self.s.soon))
                        )
                    } else if i == self.menu {
                        format!("  {} {}   {}", t.brand("▸"), t.strong(&label_padded), t.dim(desc))
                    } else {
                        format!(
                            "    {}   {}",
                            t.rgb(&label_padded, onra_ui::palette::MUTED),
                            t.faint(desc)
                        )
                    };
                    lines.push(line);
                }
                lines
            }
            Mode::Input => match &self.tab {
                Some((options, current)) => {
                    let shown: Vec<String> = options
                        .iter()
                        .enumerate()
                        .map(|(i, o)| {
                            let last = o.rsplit(' ').next().unwrap_or(o);
                            if i == *current { t.brand(last) } else { t.faint(last) }
                        })
                        .collect();
                    vec![format!("  {}", shown.join("  "))]
                }
                None => Vec::new(),
            },
            Mode::PickAsset => {
                let mut lines = vec![format!("  {}", t.strong(self.s.pick_asset))];
                for (i, symbol) in self.asset_options().iter().enumerate() {
                    lines.push(if i == self.pick {
                        format!("  {} {}", t.brand("▸"), t.strong(symbol))
                    } else {
                        format!("    {}", t.rgb(symbol, onra_ui::palette::MUTED))
                    });
                }
                lines
            }
            Mode::PickVenue { asset } => {
                let mut lines =
                    vec![format!("  {}  {}", t.strong(self.s.pick_venue), t.brand(asset))];
                let options = std::iter::once((None, self.s.all_venues))
                    .chain(VenueId::ALL.iter().map(|v| (Some(*v), v.name())));
                for (i, (venue, label)) in options.enumerate() {
                    let dot =
                        venue.map_or_else(|| t.brand("◆"), |v| t.rgb("●", price::venue_color(v)));
                    lines.push(if i == self.pick {
                        format!("  {} {dot} {}", t.brand("▸"), t.strong(label))
                    } else {
                        format!("    {dot} {}", t.dim(label))
                    });
                }
                lines
            }
            Mode::Watching { watch, interval, .. } => watch.render(t, *interval, true),
            Mode::Busy { started, loader, .. } => {
                let elapsed = started.elapsed();
                if elapsed < price::LOADER_DELAY { Vec::new() } else { loader.lines(t, elapsed) }
            }
        }
    }

    /// The styled input line and the cursor's column within it.
    fn input_line(&self) -> (String, usize) {
        let t = &self.theme;
        let prompt = match &self.mode {
            Mode::Busy { started, .. } => {
                let frames = ['◜', '◠', '◝', '◞', '◡', '◟'];
                t.brand(
                    &frames[(started.elapsed().as_millis() / 90) as usize % frames.len()]
                        .to_string(),
                )
            }
            _ => t.brand("❯"),
        };
        let typed = self.editor.text();
        let content = match &self.mode {
            Mode::Menu if typed.is_empty() => t.faint(self.s.placeholder),
            Mode::PickAsset if typed.is_empty() => t.faint(self.s.pick_asset_hint),
            Mode::PickVenue { .. } | Mode::Busy { .. } | Mode::Watching { .. } => String::new(),
            _ => {
                let ghost =
                    self.suggestion().map(|s| s[typed.len()..].to_string()).unwrap_or_default();
                format!("{}{}", t.strong(&typed), t.faint(&ghost))
            }
        };
        (format!(" {prompt} {content}"), self.editor.cursor_col())
    }

    fn hints(&self) -> &'static str {
        match self.mode {
            Mode::Menu => self.s.hints_menu,
            Mode::Input => self.s.hints_input,
            Mode::PickAsset | Mode::PickVenue { .. } => self.s.hints_pick,
            Mode::Busy { .. } => self.s.hints_busy,
            Mode::Watching { .. } => self.s.hints_watch,
        }
    }
}

/// Linked accounts for the shell. If the Binance secret can't be read, the
/// wallets still work and the total covers what can be reached.
fn load_accounts(config: &Config) -> onra_venues::Accounts {
    crate::account::load(config).unwrap_or_else(|_| onra_venues::Accounts {
        binance: None,
        hyperliquid: config.accounts().hyperliquid.clone(),
        solana: config.accounts().solana.clone(),
    })
}

/// Whether `onra` without arguments should open the shell rather than print
/// version info: only for a person at a terminal.
pub fn wanted(theme: Option<&Theme>) -> bool {
    use std::io::IsTerminal;
    theme.is_some() && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

#[cfg(test)]
mod tests {
    use super::*;
    use onra_ui::{ColorMode, Convention};

    fn shell(lang: Lang) -> Shell {
        let venues = Rc::new(Venues::new(&Default::default()).unwrap());
        let theme = Theme { mode: ColorMode::TrueColor, convention: Convention::East };
        Shell::new(Config::default(), None, theme, venues, lang, Editor::default())
    }

    /// Frames for every mode. With ONRA_SNAPSHOT=<dir> they are written out
    /// as ANSI files for visual review.
    #[test]
    fn frames_render_in_every_mode() {
        let mut frames: Vec<(&str, Frame)> = Vec::new();
        let mut sh = shell(Lang::Zh);
        sh.health = vec![
            (VenueId::Binance, Health::Up(Duration::from_millis(212))),
            (VenueId::Hyperliquid, Health::Up(Duration::from_millis(96))),
            (VenueId::Jupiter, Health::Down),
        ];
        frames.push(("menu", sh.frame()));
        sh.set_health(VenueId::Jupiter, Health::Up(Duration::from_millis(180)));
        frames.push(("menu-online", sh.frame()));
        sh.total = Total::Known(Some(rust_decimal::Decimal::new(1_234_567, 2)));
        frames.push(("menu-total", sh.frame()));
        sh.show_total = true;
        frames.push(("menu-total-shown", sh.frame()));
        sh.show_total = false;

        sh.mode = Mode::Input;
        sh.editor.set("pri");
        frames.push(("input-ghost", sh.frame()));
        sh.editor.set("price S");
        sh.cycle_completion();
        frames.push(("input-tab", sh.frame()));

        sh.editor.clear();
        sh.mode = Mode::PickAsset;
        sh.editor.set("B");
        frames.push(("pick-asset", sh.frame()));

        sh.editor.clear();
        sh.mode = Mode::PickVenue { asset: "SOL".into() };
        sh.pick = 1;
        frames.push(("pick-venue", sh.frame()));

        let mut loader = Loader::default();
        for v in VenueId::ALL {
            loader.apply(Progress::Expect(v));
        }
        loader.apply(Progress::Done {
            venue: VenueId::Hyperliquid,
            error: None,
            elapsed: Duration::from_millis(96),
        });
        sh.mode = Mode::Busy {
            started: Instant::now() - Duration::from_millis(400),
            loader,
            json: false,
        };
        frames.push(("busy", sh.frame()));

        let mut en = shell(Lang::En);
        en.health = vec![
            (VenueId::Binance, Health::Up(Duration::from_millis(212))),
            (VenueId::Hyperliquid, Health::Up(Duration::from_millis(96))),
            (VenueId::Jupiter, Health::Checking),
        ];
        frames.push(("menu-en", en.frame()));
        en.set_health(VenueId::Jupiter, Health::Up(Duration::from_millis(180)));
        frames.push(("menu-en-online", en.frame()));

        for (name, frame) in &frames {
            assert!(frame.lines.len() > 5, "{name} frame too short");
            if let Some((row, _)) = frame.cursor {
                assert!(row < frame.lines.len(), "{name} cursor outside frame");
            }
        }
        if let Ok(dir) = std::env::var("ONRA_SNAPSHOT") {
            std::fs::create_dir_all(&dir).unwrap();
            for (name, frame) in frames {
                std::fs::write(
                    format!("{dir}/{name}.ansi"),
                    frame.lines.join(
                        "
",
                    ),
                )
                .unwrap();
            }
        }
    }
}

<p align="center"><img src="assets/logo.png" alt="onra" width="360"></p>
<p align="center"><code>M A R K E T S &nbsp; N E V E R &nbsp; S L E E P</code></p>

**Onra is the agent-native trading terminal for on-chain markets.** One
command line, used by humans and AI agents alike, that reaches every market:
DEXs, CEXs, perpetuals, tokenized stocks.

Traditional markets close. On-chain markets never do. Onra is the sun that
never sets over them.

> **Status: pre-alpha, building in public.** Read-only market data works
> today. Trading, the policy engine and MCP integration are on the roadmap
> below. Nothing here can move funds yet.

## Why

AI agents are fluent in the command line. Exchanges speak a dozen
incompatible APIs. Onra sits between them: one consistent interface, styled
tables for people, a stable JSON contract for machines, and a safety layer
so agents can trade without ever touching a private key.

Onra is infrastructure. It is not an agent, a strategy, or a bot. It is the
entry point that agents and people use to reach markets.

## A taste

```console
$ onra price SOL ETH

  SOL   71.2289  +3.77%                             24h  67.41 ────────●───── 73.28
    ● binance       SOL/USDT    71.2500   +3.77%   ▄▄▃▃▂▁▄▅▆▇█▇▄▄▅▅▄▅▆▆▇█▆▄   219ms
    ● hyperliquid   SOL-PERP    71.2035   +3.81%   ▄▄▃▃▂▁▄▅▆▇█▇▄▄▄▅▄▅▆▆▇█▆▄   681ms
    ● jupiter       SOL         71.2289   +3.77%                              166ms
    spread 0.07%  ·  low hyperliquid  ·  high binance

  ETH   1,809.99  +4.68%                      24h  1,711.08 ──────────●─── 1,845.18
    ● binance       ETH/USDT   1,809.99   +4.68%   ▅▆▅▃▂▁▅▆▇▆█▇▄▅▅▅▅▆▇▆▇█▅▅   217ms
    ● hyperliquid   ETH-PERP   1,808.75   +4.85%   ▅▆▅▃▂▁▅▆▇▆█▇▄▅▅▅▅▇▇▆▇█▅▅   423ms
    ● jupiter       ETH        1,816.01   +5.35%                              225ms
    spread 0.40%  ·  low hyperliquid  ·  high jupiter

  3 venues · 683ms · 01:33:08 UTC
```

On a terminal the view is alive: venues report in as they answer, rows unfold,
prices carry their own up/down colors. Each asset shows a reference price
(the median across venues), its 24h range, and the spread between the
cheapest and the dearest venue.

The same command, piped or called by an agent, emits JSON:

```console
$ onra price SOL@hl | jq .
{
  "ok": true,
  "data": {
    "assets": [
      {
        "query": "SOL@hl",
        "reference_price": "71.2175",
        "quotes": [
          {
            "market": { "venue": "hyperliquid", "symbol": "SOL-PERP", "kind": "perp", ... },
            "price": "71.2175",
            "change_24h_pct": "3.8262",
            "low_24h": "67.504",
            "high_24h": "73.24",
            "trend_24h": ["71.313", "71.327", ...],
            "latency_ms": 632
          }
        ]
      }
    ]
  },
  "meta": { "v": 1, "ts": "2026-11-16T01:33:08Z", "latency_ms": 635 }
}
```

With several venues, each asset also carries `spread_pct`, `lowest` and
`highest`: the cross-venue comparison, precomputed for agents.

## Markets

One identifier scheme across every venue:

```
<base>[/<quote>][-PERP][@<venue>]
```

| Identifier    | Meaning                          |
|---------------|----------------------------------|
| `SOL`         | SOL on every venue that lists it |
| `SOL@jup`     | SOL spot on Jupiter              |
| `ETH-PERP@hl` | ETH perpetual on Hyperliquid     |
| `BTC/USDT@bn` | BTC/USDT spot on Binance         |
| `<mint>@jup`  | A Solana token by mint address   |

Venues today: **Binance** (`bn`), **Hyperliquid** (`hl`), **Jupiter** (`jup`).

Bare symbols only resolve to verified tokens. Solana is full of tokens
impersonating real tickers; unverified tokens are reachable by mint address
and are always flagged.

## The machine contract

Agents get a stable, versioned interface:

- **Format.** `--format auto` (the default) renders tables on a terminal and
  JSON everywhere else. Force either with `--json` or `--format table|plain`.
- **Numbers are strings.** No floating-point surprises.
- **Errors are structured**, with a stable `code`, a `retryable` flag and a
  machine-readable `hint`, usually a command to run next.
- **Partial results.** If one venue is down, the others still answer, and the
  failure is reported in `warnings`.

| Exit code | Meaning                                    |
|-----------|--------------------------------------------|
| 0         | Success (possibly with warnings)           |
| 1         | General error (config, internal)           |
| 2         | Usage error                                |
| 3         | Blocked by policy *(planned)*              |
| 4         | Needs human approval *(planned)*           |
| 5         | Venue error, or market not found           |
| 6         | Network error, retryable                   |
| 7         | Plan expired *(planned)*                   |

## Configuration

```console
$ onra config set network.proxy http://127.0.0.1:7890   # route through a proxy
$ onra config set ui.color_convention east              # red up, green down
$ onra config show
```

Many networks block some venues. Onra honors `--proxy`, then `ONRA_PROXY`,
then the config file, then the standard `HTTPS_PROXY` / `ALL_PROXY`
variables. `http://`, `https://` and `socks5://` proxies all work.

Up/down colors default from your system locale: Chinese, Japanese and Korean
locales show gains in red, as their markets do.

## Roadmap

| Milestone      | Scope                                                                         |
|----------------|-------------------------------------------------------------------------------|
| **M0 Foundation** ✓ | Workspace, output contract, rendering, brand. `onra price` across three venues |
| **M1 Read-only**    | Balances, positions and orders with credentials; order books; live `watch`    |
| **M2 Trading**      | Plan → simulate → policy check → execute; local signer daemon; audit log      |
| **M3 Agents**       | MCP server, scoped agent identities, human approval flow, `onra schema`        |
| **M4 Dashboard**    | Full-screen TUI                                                               |

The safety model that M2 builds toward: Onra is **non-custodial**, agents
**never see private keys** (a local signer holds them), every trade is a
**plan** that is simulated and checked against your policy (limits,
allowlists, slippage, leverage) before anything is signed, and a single
`onra halt` freezes every agent.

## Build from source

Requires Rust 1.85 or newer.

```console
$ git clone https://github.com/yu7dan/onra
$ cd onra
$ cargo build --release
$ ./target/release/onra price SOL
```

## License

[Business Source License 1.1](LICENSE). Free for personal and other
non-commercial use, including trading your own assets and powering your own
agents. Offering Onra or a derivative as a commercial product or service
needs a commercial license. Each version converts to Apache 2.0 four years
after release.

# trading-eng

A desktop **order-book visualizer** built with [Tauri](https://tauri.app). It drives a real
matching-engine `Book` with a self-running market simulator and renders a live, depth-shaded
bid/ask ladder — anchored to the real last price of any ticker you load.

![Order book visualizer](images/Screenshot%202026-07-12%20at%2018.36.42.png)

## What it does

- **Live depth ladder** — asks on top (highest price down to the best offer), bids below (best
  bid down), each row shaded by resting quantity so you can read liquidity at a glance.
- **Spread bar** — best bid, best ask, mid, absolute spread, and spread in basis points,
  updated on every tick.
- **Real-price anchoring** — type a symbol (AAPL, MSFT, NVDA, TSLA, BTC-USD, …) and hit **Load**.
  The app fetches the latest price from Yahoo Finance's keyless chart API and re-seeds the book
  around it. Cheap and expensive tickers both behave, since the drift band scales to the anchor.
- **Playback controls** — **Start/Stop** the simulation, **Reset** to reseed, and a **Speed**
  slider (60–1000 ms per tick).

## How it works

The Rust backend (`src-tauri/src/lib.rs`) owns a bid `Book` and an ask `Book` from the
external `matching_engine` (skiplist) crate, behind a `Mutex`. On each tick the simulator:

1. random-walks the mid within an anchor-relative band,
2. adds a few resting bids strictly below the mid and asks strictly above it,
3. cancels any order that ended up on the wrong side of the drift (`enforce_partition`), and
4. trims each side back under a depth cap.

This is a **resting-only** book — bids never cross asks, so the spread stays positive; no
matching is performed. Prices live on a 1-cent grid tracked as integer cents, and levels are
serialized as `Decimal` strings to avoid float rounding.

The frontend (`src/`, vanilla JS) polls the backend over Tauri `invoke` commands:

| Command         | Purpose                                              |
| --------------- | ---------------------------------------------------- |
| `book_snapshot` | read-only view of the current book                   |
| `sim_tick`      | advance the simulator one step, return the snapshot  |
| `reset`         | clear both books and reseed at the current anchor    |
| `set_symbol`    | fetch a ticker's latest price and re-anchor the book |

## Getting started

Requirements: [Rust](https://rustup.rs) and the
[Tauri prerequisites](https://tauri.app/start/prerequisites/), plus Node for the Tauri CLI.

```bash
npm install

# run in development
npx tauri dev

# build a release bundle
npx tauri build
```

> Note: `src-tauri/Cargo.toml` depends on the `matching_engine` crate via a local path
> (`../../../matching-engine`), so that repo must be checked out alongside this one.

## Tests

```bash
cd src-tauri
cargo test                # includes sim_keeps_bid_below_ask (book never crosses)
cargo test -- --ignored   # network-gated Yahoo price fetch
```

## Project layout

```
src/                  frontend — index.html, main.js, styles.css
src-tauri/src/lib.rs  backend — simulator, book state, Tauri commands
src-tauri/            Tauri config, Rust crate, icons
images/               screenshots
```

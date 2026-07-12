// Order-book visualizer backend.
//
// Owns a bid `Book` and an ask `Book` from the `matching_engine` crate directly
// (behind a `Mutex`), drives them with a self-running random simulator, and maps
// the engine's `view()` into serializable snapshots for the frontend.
//
// This is a *matching* book: every incoming order is run through the opposite
// side's `book_matcher` first, executing against any crossing resting liquidity
// in price-time priority, and only the unfilled remainder rests. Marketable
// orders therefore print trades and consume depth, while the book itself stays
// uncrossed (a resting bid can only exist after all asks at or below its price
// have been eaten).

use std::sync::Mutex;

use matching_engine::book::Book;
use matching_engine::order::{CancelOrder, NewOrder, Side};
use matching_engine::view::BookView;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::Serialize;

/// Prices live on a 1-cent grid, tracked internally as integer cents so orders
/// always land exactly on a tick.
const TICK_CENTS: i64 = 1;
/// Soft cap on resting orders per side; above this we cancel to keep depth bounded.
const MAX_ORDERS_PER_SIDE: usize = 120;
/// Default anchor when no stock is loaded: 100.00.
const DEFAULT_ANCHOR_CENTS: i64 = 10_000;

struct EngineState {
    bids: Book,
    asks: Book,
    next_id: u64,
    symbol: String,
    anchor_cents: i64,
    mid_cents: i64,
    floor_cents: i64,
    ceil_cents: i64,
    /// Price (in cents) of the most recent trade print, if any has occurred.
    last_trade_cents: Option<i64>,
    /// Quantity of the most recent trade print.
    last_trade_qty: i32,
    /// Cumulative traded quantity since the book was seeded.
    volume: i64,
    /// Number of trade prints since the book was seeded.
    trades: u64,
}

impl EngineState {
    fn new() -> Self {
        Self::seeded("SIM", DEFAULT_ANCHOR_CENTS)
    }

    /// Build a fresh book anchored at `anchor_cents` and seed some liquidity so
    /// the first render isn't empty. The drift band scales to the anchor so it
    /// behaves for both cheap and expensive stocks.
    fn seeded(symbol: &str, anchor_cents: i64) -> Self {
        let anchor_cents = anchor_cents.max(1);
        // Allow the mid to wander ~20% of the anchor (at least 5.00) each way.
        let band = (anchor_cents / 5).max(500);
        let mut state = EngineState {
            bids: Book::new(Side::Buy, None, None, None),
            asks: Book::new(Side::Sell, None, None, None),
            next_id: 1,
            symbol: symbol.to_uppercase(),
            anchor_cents,
            mid_cents: anchor_cents,
            floor_cents: (anchor_cents - band).max(1),
            ceil_cents: anchor_cents + band,
            last_trade_cents: None,
            last_trade_qty: 0,
            volume: 0,
            trades: 0,
        };
        for _ in 0..8 {
            state.tick();
        }
        state
    }

    fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn cents(cents: i64) -> Decimal {
        Decimal::new(cents, 2)
    }

    /// All live order ids in a book, read from its view.
    fn side_ids(book: &Book) -> Vec<u64> {
        BookView::from_book(book)
            .levels
            .iter()
            .flat_map(|l| l.orders.iter().map(|o| o.id))
            .collect()
    }

    /// Advance the simulation by one step: drift the mid, post fresh passive
    /// liquidity on each side, fire a few marketable orders that cross the spread
    /// and print trades, then trim depth back under its cap.
    fn tick(&mut self) {
        // Random walk the mid, clamped to the anchor-relative band.
        let drift = rand::random_range(-2..=2i64) * TICK_CENTS;
        self.mid_cents = (self.mid_cents + drift).clamp(self.floor_cents, self.ceil_cents);

        // Passive makers: bids below the mid, asks above it. These add depth and
        // rest (nothing to cross), unless drift has left stale liquidity on the
        // far side — in which case they match it.
        let n_bids = rand::random_range(1..=4);
        for _ in 0..n_bids {
            let offset = rand::random_range(1..=15) * TICK_CENTS;
            let qty = rand::random_range(1..=100);
            self.submit(Side::Buy, self.mid_cents - offset, qty);
        }
        let n_asks = rand::random_range(1..=4);
        for _ in 0..n_asks {
            let offset = rand::random_range(1..=15) * TICK_CENTS;
            let qty = rand::random_range(1..=100);
            self.submit(Side::Sell, self.mid_cents + offset, qty);
        }

        // Aggressive takers: orders that reach across the mid and lift/hit the
        // far side, executing against resting liquidity and printing trades.
        let n_aggr = rand::random_range(0..=2);
        for _ in 0..n_aggr {
            let reach = rand::random_range(0..=8) * TICK_CENTS;
            let qty = rand::random_range(1..=80);
            if rand::random_bool(0.5) {
                self.submit(Side::Buy, self.mid_cents + reach, qty);
            } else {
                self.submit(Side::Sell, self.mid_cents - reach, qty);
            }
        }

        self.trim_side(Side::Buy);
        self.trim_side(Side::Sell);
    }

    /// Submit one order into the matching engine: cross it against the opposite
    /// book first (executing any marketable quantity in price-time priority),
    /// then rest whatever is left on its own side. Records a trade print for the
    /// filled quantity, priced at the resting (maker) level it lifted.
    fn submit(&mut self, side: Side, price_cents: i64, qty: i32) {
        if qty <= 0 {
            return;
        }
        let price = Self::cents(price_cents);
        let id = self.alloc_id();

        // Best price on the side we're about to hit, captured before it moves.
        let maker_cents = match side {
            Side::Buy => Self::best_cents(&self.asks),
            Side::Sell => Self::best_cents(&self.bids),
        };

        let unfilled = match side {
            Side::Buy => self.asks.book_matcher(&price, qty),
            Side::Sell => self.bids.book_matcher(&price, qty),
        };

        let filled = qty - unfilled;
        if filled > 0 {
            self.last_trade_cents = maker_cents.or(Some(price_cents));
            self.last_trade_qty = filled;
            self.volume += filled as i64;
            self.trades += 1;
        }

        if unfilled > 0 {
            let book = match side {
                Side::Buy => &mut self.bids,
                Side::Sell => &mut self.asks,
            };
            book.submit_order(NewOrder {
                id,
                side,
                price,
                qty: unfilled,
            });
        }
    }

    /// Best (top-of-book) price on `book`, in integer cents, if it has liquidity.
    fn best_cents(book: &Book) -> Option<i64> {
        BookView::from_book(book)
            .levels
            .first()
            .and_then(|l| (l.price * Decimal::from(100)).to_i64())
    }

    /// Cancel random resting orders until the side is back under its cap.
    fn trim_side(&mut self, side: Side) {
        let book = match side {
            Side::Buy => &mut self.bids,
            Side::Sell => &mut self.asks,
        };
        let mut ids = Self::side_ids(book);
        while ids.len() > MAX_ORDERS_PER_SIDE {
            let idx = rand::random_range(0..ids.len());
            let id = ids.swap_remove(idx);
            book.cancel_order(CancelOrder { id });
        }
    }

    fn snapshot(&self, depth: usize) -> BookSnapshot {
        // The view returns levels best-price-first: bids high->low, asks low->high.
        let bid_view = BookView::from_book(&self.bids);
        let ask_view = BookView::from_book(&self.asks);

        let best_bid = bid_view.levels.first().map(|l| l.price);
        let best_ask = ask_view.levels.first().map(|l| l.price);

        let bids: Vec<LevelDto> = bid_view
            .levels
            .iter()
            .take(depth)
            .map(LevelDto::from)
            .collect();
        let asks: Vec<LevelDto> = ask_view
            .levels
            .iter()
            .take(depth)
            .map(LevelDto::from)
            .collect();

        let max_qty = bids
            .iter()
            .chain(asks.iter())
            .map(|l| l.qty)
            .max()
            .unwrap_or(0);

        let (spread, spread_bps, mid) = match (best_bid, best_ask) {
            (Some(bb), Some(ba)) => {
                let spread = ba - bb;
                let mid = (bb + ba) / Decimal::from(2);
                let bps = if mid > Decimal::ZERO {
                    (spread / mid * Decimal::from(10_000)).to_f64()
                } else {
                    None
                };
                (Some(spread.to_string()), bps, Some(mid.to_string()))
            }
            _ => (None, None, None),
        };

        BookSnapshot {
            symbol: self.symbol.clone(),
            last_price: Self::cents(self.anchor_cents).to_string(),
            bids,
            asks,
            best_bid: best_bid.map(|d| d.to_string()),
            best_ask: best_ask.map(|d| d.to_string()),
            spread,
            spread_bps,
            mid,
            max_qty,
            last_trade: self.last_trade_cents.map(|c| Self::cents(c).to_string()),
            last_trade_qty: self.last_trade_qty,
            volume: self.volume,
            trades: self.trades,
        }
    }
}

/// One price level as sent to the frontend. Prices are stringified `Decimal`s to
/// avoid float rounding.
#[derive(Serialize)]
struct LevelDto {
    price: String,
    qty: i32,
    orders: usize,
}

impl From<&matching_engine::view::PriceLevelView> for LevelDto {
    fn from(l: &matching_engine::view::PriceLevelView) -> Self {
        LevelDto {
            price: l.price.to_string(),
            qty: l.total_qty,
            orders: l.orders.len(),
        }
    }
}

/// Full book view: bids best-first (high->low), asks best-first (low->high).
#[derive(Serialize)]
struct BookSnapshot {
    /// Ticker currently simulated (e.g. "AAPL", or "SIM" when none loaded).
    symbol: String,
    /// Real latest price the book was anchored at, as a dollar string.
    last_price: String,
    bids: Vec<LevelDto>,
    asks: Vec<LevelDto>,
    best_bid: Option<String>,
    best_ask: Option<String>,
    spread: Option<String>,
    spread_bps: Option<f64>,
    mid: Option<String>,
    max_qty: i32,
    /// Last trade print (maker price) as a dollar string, if any trade happened.
    last_trade: Option<String>,
    /// Quantity of the last trade print.
    last_trade_qty: i32,
    /// Cumulative traded quantity since the book was seeded.
    volume: i64,
    /// Number of trade prints since the book was seeded.
    trades: u64,
}

/// Read-only view of the current book — the "view API" the frontend polls.
#[tauri::command]
fn book_snapshot(state: tauri::State<'_, Mutex<EngineState>>, depth: usize) -> BookSnapshot {
    state.lock().unwrap().snapshot(depth)
}

/// Advance the simulator one step and return the fresh snapshot.
#[tauri::command]
fn sim_tick(state: tauri::State<'_, Mutex<EngineState>>, depth: usize) -> BookSnapshot {
    let mut state = state.lock().unwrap();
    state.tick();
    state.snapshot(depth)
}

/// Clear both books and reseed at the current symbol/anchor.
#[tauri::command]
fn reset(state: tauri::State<'_, Mutex<EngineState>>, depth: usize) -> BookSnapshot {
    let mut guard = state.lock().unwrap();
    let symbol = guard.symbol.clone();
    *guard = EngineState::seeded(&symbol, guard.anchor_cents);
    guard.snapshot(depth)
}

/// Fetch a stock's latest price, then re-anchor the book around it.
#[tauri::command]
async fn set_symbol(
    state: tauri::State<'_, Mutex<EngineState>>,
    symbol: String,
    depth: usize,
) -> Result<BookSnapshot, String> {
    let price = fetch_latest_price(&symbol).await?;
    let anchor_cents = (price * 100.0).round() as i64;
    if anchor_cents <= 0 {
        return Err(format!("no valid price for {symbol}"));
    }
    let mut guard = state.lock().unwrap();
    *guard = EngineState::seeded(&symbol, anchor_cents);
    Ok(guard.snapshot(depth))
}

/// Fetch the latest price for `symbol` from Yahoo Finance's keyless chart API,
/// reading `chart.result[0].meta.regularMarketPrice`.
async fn fetch_latest_price(symbol: &str) -> Result<f64, String> {
    let ticker = symbol.trim().to_uppercase();
    if ticker.is_empty() {
        return Err("empty symbol".into());
    }
    let url = format!(
        "https://query1.finance.yahoo.com/v8/finance/chart/{ticker}?interval=1d&range=1d"
    );

    // Yahoo rejects requests without a browser-like User-Agent.
    let resp = reqwest::Client::new()
        .get(&url)
        .header("User-Agent", "Mozilla/5.0")
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;

    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("read failed: {e}"))?;

    if !body["chart"]["error"].is_null() {
        return Err(format!("unknown symbol: {ticker}"));
    }
    body["chart"]["result"][0]["meta"]["regularMarketPrice"]
        .as_f64()
        .ok_or_else(|| format!("no price available for {ticker}"))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(Mutex::new(EngineState::new()))
        .invoke_handler(tauri::generate_handler![
            book_snapshot,
            sim_tick,
            reset,
            set_symbol
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Network-gated: hits Yahoo for a real price. Run with
    /// `cargo test -- --ignored fetch_real_price`.
    #[tokio::test]
    #[ignore]
    async fn fetch_real_price() {
        let price = fetch_latest_price("AAPL").await.expect("AAPL price");
        assert!(price > 0.0, "expected a positive price, got {price}");

        let err = fetch_latest_price("ZZZZNOTREAL").await;
        assert!(err.is_err(), "bogus symbol should error");
    }

    /// Matching keeps the book uncrossed on its own: a resting bid can only exist
    /// once every ask at or below its price has been consumed, so best bid always
    /// stays strictly below best ask and the spread stays positive.
    #[test]
    fn sim_keeps_bid_below_ask() {
        let mut state = EngineState::new();
        for _ in 0..500 {
            state.tick();
            let snap = state.snapshot(20);
            if let (Some(bb), Some(ba)) = (&snap.best_bid, &snap.best_ask) {
                let bb: Decimal = bb.parse().unwrap();
                let ba: Decimal = ba.parse().unwrap();
                assert!(bb < ba, "book crossed: best_bid {bb} >= best_ask {ba}");
            }
        }
    }

    /// Marketable orders must actually execute: over many ticks the simulator
    /// should print trades and accumulate traded volume.
    #[test]
    fn sim_prints_trades() {
        let mut state = EngineState::new();
        for _ in 0..500 {
            state.tick();
        }
        let snap = state.snapshot(20);
        assert!(snap.trades > 0, "expected trades to print");
        assert!(snap.volume > 0, "expected traded volume to accumulate");
        assert!(snap.last_trade.is_some(), "expected a last trade price");
    }
}

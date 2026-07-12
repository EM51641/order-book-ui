const { invoke } = window.__TAURI__.core;

const DEPTH = 12;

let timer = null;
let intervalMs = 400;

// element handles, wired on load
let el = {};

async function tick() {
  const snap = await invoke("sim_tick", { depth: DEPTH });
  render(snap);
}

async function refresh() {
  const snap = await invoke("book_snapshot", { depth: DEPTH });
  render(snap);
}

function rowsHtml(levels, side, maxQty) {
  return levels
    .map((l) => {
      const w = maxQty > 0 ? (l.qty / maxQty) * 100 : 0;
      return `<div class="row">
        <div class="bar" style="width:${w.toFixed(1)}%"></div>
        <span class="price">${l.price}</span>
        <span class="qty">${l.qty}</span>
      </div>`;
    })
    .join("");
}

function render(snap) {
  const maxQty = snap.max_qty || 0;

  // asks come low->high; show highest at top, best ask nearest the spread.
  const asks = [...snap.asks].reverse();
  el.asks.innerHTML = rowsHtml(asks, "ask", maxQty);
  // bids come high->low, which is already the display order.
  el.bids.innerHTML = rowsHtml(snap.bids, "bid", maxQty);

  el.symbolChip.textContent = snap.symbol ?? "SIM";
  el.lastPrice.textContent = snap.last_price ? `anchor $${snap.last_price}` : "";

  el.bestBid.textContent = snap.best_bid ?? "—";
  el.bestAsk.textContent = snap.best_ask ?? "—";
  el.mid.textContent = snap.mid ?? "—";
  el.spread.textContent = snap.spread ?? "—";
  el.spreadBps.textContent =
    snap.spread_bps != null ? `${snap.spread_bps.toFixed(1)} bps` : "—";
}

async function loadSymbol(symbol) {
  const sym = symbol.trim().toUpperCase();
  if (!sym) return;
  el.loadMsg.textContent = `loading ${sym}…`;
  el.loadMsg.classList.remove("error");
  try {
    const snap = await invoke("set_symbol", { symbol: sym, depth: DEPTH });
    render(snap);
    el.loadMsg.textContent = "";
  } catch (err) {
    el.loadMsg.textContent = String(err);
    el.loadMsg.classList.add("error");
  }
}

function start() {
  if (timer) return;
  el.dot.classList.add("live");
  el.toggle.textContent = "Stop";
  el.toggle.classList.remove("btn-start");
  el.toggle.classList.add("btn-stop");
  timer = setInterval(tick, intervalMs);
}

function stop() {
  clearInterval(timer);
  timer = null;
  el.dot.classList.remove("live");
  el.toggle.textContent = "Start";
  el.toggle.classList.remove("btn-stop");
  el.toggle.classList.add("btn-start");
}

window.addEventListener("DOMContentLoaded", () => {
  el = {
    asks: document.querySelector("#asks"),
    bids: document.querySelector("#bids"),
    symbolChip: document.querySelector("#symbol-chip"),
    lastPrice: document.querySelector("#last-price"),
    bestBid: document.querySelector("#best-bid"),
    bestAsk: document.querySelector("#best-ask"),
    mid: document.querySelector("#mid"),
    spread: document.querySelector("#spread"),
    spreadBps: document.querySelector("#spread-bps"),
    dot: document.querySelector("#status-dot"),
    toggle: document.querySelector("#toggle"),
    reset: document.querySelector("#reset"),
    speed: document.querySelector("#speed"),
    speedVal: document.querySelector("#speed-val"),
    loadForm: document.querySelector("#load-form"),
    symbolInput: document.querySelector("#symbol-input"),
    loadMsg: document.querySelector("#load-msg"),
  };

  el.toggle.addEventListener("click", () => (timer ? stop() : start()));

  el.loadForm.addEventListener("submit", (e) => {
    e.preventDefault();
    loadSymbol(el.symbolInput.value);
  });

  el.reset.addEventListener("click", async () => {
    const snap = await invoke("reset", { depth: DEPTH });
    render(snap);
  });

  el.speed.addEventListener("input", () => {
    intervalMs = Number(el.speed.value);
    el.speedVal.textContent = `${intervalMs}ms`;
    if (timer) {
      stop();
      start();
    }
  });

  refresh().then(start);
});

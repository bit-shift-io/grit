import {
  store,
  render,
  renderTabBar,
  currentRepoPath,
  setDockVisible,
  ensureFolioFrame,
  probeExternal,
  sessionIdForRepo,
  KRUST_BASE,
  KRUST_PROBE_MS,
  KRUST_SESSIONS,
  FOLIO_BASE,
} from "./common.js";

// ======================================
// View dock + krust/folio integration
// ======================================
export function showView(view) {
  store.activeView = view;
  updateUrlView(view);
  const dashboard = view === "dashboard";
  document.getElementById("actions").style.display = dashboard ? "block" : "none";
  document.getElementById("repo-view").style.display = dashboard ? "block" : "none";
  document.getElementById("branches-section").style.display = dashboard ? "block" : "none";
  document.getElementById("stashes-section").style.display = dashboard ? "block" : "none";
  document.getElementById("history-section").style.display = dashboard ? "block" : "none";
  document.getElementById("log-section").style.display = dashboard ? "block" : "none";
  setDockVisible("files-section", view === "files");
  setDockVisible("view-term-1", view === "term-1");
  for (const v of ["dashboard", "files", "term-1"]) {
    document.body.classList.toggle(`view-${v}`, v === view);
  }
  const sess = KRUST_SESSIONS[view];
  if (sess) {
    ensureKrustFrame(sess);
    // The user is looking at the terminal: any pending unseen-output mark
    // for the session now on screen is stale by definition.
    store.termUnseenSid = null;
    const frame = document.getElementById(sess.iframe);
    if (frame && frame.getAttribute("src")) {
      try { frame.contentWindow.focus(); } catch (e) { /* cross-origin focus is best-effort */ }
    }
  }
  if (view === "files") {
    ensureFolioFrame();
    const frame = document.getElementById("folio-frame");
    if (frame && frame.getAttribute("src")) {
      try { frame.contentWindow.focus(); } catch (e) { /* cross-origin focus is best-effort */ }
    }
  }
}

function updateUrlView(view) {
  const url = new URL(window.location.href);
  if (view === "dashboard") {
    url.searchParams.delete("view");
  } else {
    url.searchParams.set("view", view);
  }
  window.history.replaceState({}, "", url);
}

export function setView(view) {
  showView(view);
  if (store.lastState) render(store.lastState);
}
//#region krust terminal integration

// Session id with unseen terminal output, or null. See handleKrustActivity.

function sessionIdFor(sess) {
  return sessionIdForRepo(currentRepoPath(), sess);
}

// The session the krust iframe is currently bound to — the only one that can
// produce unseen output for this page. Returns null before the first visit.
function liveSessionId() {
  const frame = document.getElementById(KRUST_SESSIONS["term-1"].iframe);
  const src = frame && frame.getAttribute("src");
  if (!src) return null;
  const m = /[?&]s=([^&]+)/.exec(src);
  return m ? decodeURIComponent(m[1]) : null;
}

// Unseen terminal output: krust's client page pings the parent (throttled to
// 250ms) whenever its session emits PTY bytes. Krust runs on :3000 and grit
// on :5000, so the origin check below is what makes this trustworthy — only
// pings that actually came from krust count.
function handleKrustActivity(event) {
  if (event.origin !== new URL(KRUST_BASE).origin) return;
  const data = event.data;
  if (!data || data.type !== "krust:activity") return;
  // The terminal is on screen: output is expected, not news.
  if (store.activeView === "term-1") return;
  const sid = liveSessionId();
  if (!sid || sid === store.termUnseenSid) return;
  store.termUnseenSid = sid;
  if (store.lastState) renderTabBar(store.lastState);
}
function krustFrameSrc(sess, sid) {
  return `${KRUST_BASE}/?s=${sid}&dir=${encodeURIComponent(currentRepoPath())}`;
}

function ensureKrustFrame(sess) {
  const frame = document.getElementById(sess.iframe);
  if (!frame) return;
  const sid = sessionIdFor(sess);
  const current = frame.getAttribute("src");
  if (current) {
    const m = /[?&]s=([^&]+)/.exec(current);
    if (m && m[1] === sid) return;
    const oldSid = m ? m[1] : null;
    if (oldSid && store.krustAvailable) {
      try { fetch(`${KRUST_BASE}/reset?session_id=${encodeURIComponent(oldSid)}`, { mode: "cors" }); } catch (e) {}
    }
  }
  // Rebinding to another repo's session recycles the old one, so a pending
  // unseen-output mark for it can never be cleared by visiting it again.
  store.termUnseenSid = null;
  frame.setAttribute("src", krustFrameSrc(sess, sid));
}

function probeKrust() {
  probeExternal("krust", KRUST_BASE, "dashboard", (ok) => {
    store.krustAvailable = ok;
    document.querySelectorAll(".krust-btn").forEach((btn) => {
      btn.style.display = ok ? "" : "none";
    });
    if (!ok && store.activeView === "term-1") {
      setView("dashboard");
    }
  });
}
function probeFolio() {
  probeExternal("folio", FOLIO_BASE, "dashboard", (ok) => {
    store.folioAvailable = ok;
    if (store.lastState) renderTabBar(store.lastState);
    if (!ok && store.activeView === "files") {
      setView("dashboard");
    }
  });
}

// ======================================
// Boot
// ======================================
// Registers the krust activity listener and starts the folio/krust presence
// probes. Called once from app.js main() after DOM wiring, before the initial
// state message arrives.
export function initTerminal() {
  window.addEventListener("message", handleKrustActivity);
  probeFolio();
  setInterval(probeFolio, KRUST_PROBE_MS);
  probeKrust();
  setInterval(probeKrust, KRUST_PROBE_MS);
}

//#endregion

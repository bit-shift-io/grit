// ======================================
// Constants
// ======================================
export const RECONNECT_BASE_MS = 500;
export const RECONNECT_MAX_MS = 5000;
export const SCROLL_HIT_SLACK_PX = 48;
export const DIFF_MARKER_RATIO = 0.35;
export const LCS_DIFF_CELL_CAP = 4000000;
export const HISTORY_SEARCH_FILLED_MS = 300;
export const HISTORY_SEARCH_EMPTY_MS = 150;
export const BRANCH_FILTER_DEBOUNCE_MS = 150;
export const KRUST_BASE = "http://localhost:3000";
export const KRUST_PROBE_MS = 5000;
export const KRUST_SESSIONS = {
  "term-1": { iframe: "krust-1", n: 1 },
};
export const FOLIO_BASE = "http://localhost:4000";

// ======================================
// Shared mutable state
// ======================================
export const store = {
  ws: null,
  reconnectTimer: null,
  reconnectDelayMs: RECONNECT_BASE_MS,
  activeTabId: null,
  lastState: null,
  lastRevision: 0,
  expanded: { type: null, key: null, el: null },
  awaitingNewTab: false,
  browser: { dir: null, parent: null, seeding: false },
  clearedUpToSeq: 0,
  surfacedFailures: new Map(),
  pendingCommit: null,
  showAddForm: false,
  activeView: "dashboard",
  historyQuery: "",
  historySearchTimer: null,
  knownTabIds: new Set(),
  commitCache: new Map(),
  pairCache: new Map(),
  branchFilterTimer: null,
  folioAvailable: false,
  krustAvailable: false,
  termUnseenSid: null,
};

// ======================================
// Pure helpers
// ======================================

export function getTabNameFromPath(path) {
  const parts = path.trim().split(/[/\\]/).filter(p => p.length > 0);
  const name = parts[parts.length - 1] || "repo";
  return name.replace(/-/g, " ");
}

export function getTabNameFromUrl(url) {
  let clean = url.trim();
  // Handle SSH URLs: git@github.com:user/repo.git or git@github.com:user/repo
  if (clean.startsWith("git@")) {
    // Extract after the colon: user/repo.git
    const afterColon = clean.split(":")[1] || "";
    // Remove .git suffix and take the last path segment
    const repoName = afterColon.replace(/\.git$/, "").split("/").pop();
    return repoName || null;
  }
  // Handle HTTPS URLs: https://github.com/user/repo.git
  try {
    const parsed = new URL(clean);
    const path = parsed.pathname || "";
    const repoName = path.replace(/\.git$/, "").replace(/^\/+\//, "").split("/").pop();
    return repoName || null;
  } catch (_) {
    return null;
  }
}

export function lineCounts(ins, del) {
  const parts = [];
  if (ins > 0) parts.push(`+${ins}`);
  if (del > 0) parts.push(`-${del}`);
  return parts.join("/");
}

export function splitLines(text) {
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

export function alignLines(a, b) {
  const al = splitLines(a);
  const bl = splitLines(b);
  const n = al.length;
  const m = bl.length;

  let dp = null;
  if (n * m <= LCS_DIFF_CELL_CAP) {
    dp = Array.from({ length: n + 1 }, () => new Uint32Array(m + 1));
    for (let i = n - 1; i >= 0; i--) {
      for (let j = m - 1; j >= 0; j--) {
        dp[i][j] =
          al[i] === bl[j] ? dp[i + 1][j + 1] + 1 : Math.max(dp[i + 1][j], dp[i][j + 1]);
      }
    }
  }

  const rows = [];
  let i = 0;
  let j = 0;
  if (dp) {
    while (i < n && j < m) {
      if (al[i] === bl[j]) {
        rows.push({
          left: { num: i + 1, text: al[i], type: "same" },
          right: { num: j + 1, text: bl[j], type: "same" },
        });
        i++;
        j++;
      } else if (dp[i + 1][j] >= dp[i][j + 1]) {
        rows.push({ left: { num: i + 1, text: al[i], type: "del" }, right: null });
        i++;
      } else {
        rows.push({ left: null, right: { num: j + 1, text: bl[j], type: "add" } });
        j++;
      }
    }
  } else {
    const max = Math.max(n, m);
    for (let k = 0; k < max; k++) {
      const left = k < n ? { num: k + 1, text: al[k], type: "same" } : null;
      const right = k < m ? { num: k + 1, text: bl[k], type: "same" } : null;
      if (left && right && left.text !== right.text) {
        left.type = "del";
        right.type = "add";
      }
      rows.push({ left, right });
    }
  }
  while (i < n) {
    rows.push({ left: { num: i + 1, text: al[i], type: "del" }, right: null });
    i++;
  }
  while (j < m) {
    rows.push({ left: null, right: { num: j + 1, text: bl[j], type: "add" } });
    j++;
  }
  return rows;
}

export function repoScope(path) {
  if (!path) return "root";
  const parts = path.replace(/\/+$/, "").split("/");
  const base = parts[parts.length - 1].replace(/[^\w.-]/g, "_") || "repo";
  let h = 0;
  for (let i = 0; i < path.length; i++) {
    h = ((h << 5) - h + path.charCodeAt(i)) | 0;
  }
  return base + "-" + Math.abs(h).toString(36);
}

export function sessionIdForRepo(path, sess) {
  return `grit-${repoScope(path)}-term-${sess.n}`;
}

// Probes an external service (folio/krust) on a fixed cadence. `onResult(ok)`
// applies the per-service UI updates (availability flag, button visibility,
// view fallback when a service-dependent view is active but the service is down).
export async function probeExternal(name, url, fallbackView, onResult) {
  let ok = false;
  try {
    const res = await fetch(`${url}/`, { mode: "cors" });
    ok = res.ok;
  } catch (e) {
    ok = false;
  }
  onResult(ok);
}

// ======================================
// Shared section toggling
// ======================================

// Toggles the single expandable section identified by (type, key). Returns
// true when the section is now expanded, false when it was collapsed. The
// caller is responsible for its section-specific DOM after the toggle;
// renderFn (when given) is invoked after the state change to re-render.
export function toggleSection(type, key, el, renderFn) {
  const isOpen = store.expanded.type === type && store.expanded.key === key;
  // Collapse whatever was previously expanded (if any other section/element).
  if (store.expanded.el && !isOpen) {
    const prev = store.expanded.el;
    prev.style.display = "none";
    prev.textContent = "";
    const prevRow = prev.closest(".change-row");
    if (prevRow) prevRow.classList.remove("open");
  }
  if (isOpen) {
    store.expanded.type = null;
    store.expanded.key = null;
    store.expanded.el = null;
    if (renderFn) renderFn();
    return false;
  }
  store.expanded.type = type;
  store.expanded.key = key;
  store.expanded.el = el;
  if (renderFn) renderFn();
  return true;
}

// ======================================
// Late-bound handlers
// ======================================
// common.js is the import leaf: it provides the core and imports nothing from
// app.js / diff.js / terminal.js. Two core routines nevertheless have to invoke
// implementations that live in sibling modules — render() switches dock views
// via showView() (terminal.js) and appendChangeRow() loads a file diff via
// showDiff() (diff.js). They are bound here once at startup by app.js main()
// through registerCoreHandlers() so the module graph stays acyclic.
let showView = () => {};
let showDiff = () => {};

export function registerCoreHandlers(handlers) {
  if (handlers.showView) showView = handlers.showView;
  if (handlers.showDiff) showDiff = handlers.showDiff;
}

// ======================================
// Connection
// ======================================
function scheduleReconnect(delay) {
  if (store.reconnectTimer !== null) return;
  store.reconnectTimer = setTimeout(() => {
    store.reconnectTimer = null;
    openSocket();
  }, delay);
}

export function openSocket() {
  const socket = new WebSocket(`ws://${location.host}/ws`);
  store.ws = socket;
  socket.onopen = () => {
    store.reconnectDelayMs = RECONNECT_BASE_MS;
    setConnStatus(true);
  };
  socket.onmessage = handleStateMessage;
  socket.onclose = () => {
    if (store.ws === socket) setConnStatus(false);
    scheduleReconnect(store.reconnectDelayMs);
    store.reconnectDelayMs = Math.min(store.reconnectDelayMs * 2, RECONNECT_MAX_MS);
  };
}

function setConnStatus(connected) {
  if (connected) {
    document.getElementById("conn-status")?.remove();
    return;
  }
  if (document.getElementById("conn-status")) return;
  const banner = document.createElement("div");
  banner.id = "conn-status";
  banner.textContent = "Grit daemon unreachable — retrying...";
  document.body.prepend(banner);
}

export function sendRaw(payload) {
  if (!store.ws || store.ws.readyState !== WebSocket.OPEN) return;
  store.ws.send(payload);
}
document.addEventListener("visibilitychange", () => {
  if (document.hidden) return;
  if (store.ws && (store.ws.readyState === WebSocket.OPEN || store.ws.readyState === WebSocket.CONNECTING)) return;
  if (store.reconnectTimer !== null) {
    clearTimeout(store.reconnectTimer);
    store.reconnectTimer = null;
  }
  openSocket();
});

window.addEventListener("pageshow", (event) => {
  if (!event.persisted) return;
  if (store.ws && store.ws.readyState !== WebSocket.CLOSED) return;
  openSocket();
});
// Captured once at module load, before app.js main() calls
// showView("dashboard") and rewrites the URL: updateUrlView("dashboard") deletes
// ?view, and once it is gone the first state message cannot recover it — the
// dock iframe would then be left with no src and the terminal would come up
// blank on first load only.
const initialUrlView = (function () {
  const v = new URL(window.location.href).searchParams.get("view");
  return v && ["dashboard", "files", "term-1"].indexOf(v) !== -1 ? v : null;
})();
//#region Add-repo form ("+" is a client-local mode, never a server tab)

// ======================================
// Add Repo Form
// ======================================
function setupAddRepoForm(tab) {
  const nameInput = document.getElementById("new-repo-name");
  const pathInput = document.getElementById("new-repo-path");
  const urlInput = document.getElementById("new-repo-url");
  const errorEl = document.getElementById("new-repo-error");

  if (nameInput.dataset.tabId !== String(tab.id)) {
    nameInput.dataset.tabId = String(tab.id);
    nameInput.value = "";
    pathInput.value = "";
    urlInput.value = "";
    delete nameInput.dataset.userSet;
    errorEl.style.display = "none";
    store.browser.dir = null;
    document.getElementById("folder-browser").style.display = "block";
  }

  const browserCurrent = document.getElementById("browser-current");
  const browserEntries = document.getElementById("browser-entries");
  const upBtn = document.getElementById("browser-up-btn");
  const homeBtn = document.getElementById("browser-home-btn");
  const openBtn = document.getElementById("open-repo-btn");
  const cancelBtn = document.getElementById("cancel-repo-btn");

  nameInput.oninput = () => { nameInput.dataset.userSet = "true"; };
  urlInput.oninput = () => { urlInput.dataset.userSet = "true"; };

  // The path and name fields follow the browser's current folder so the
  // user can just hit "Open Repository" without a separate select step.
  function applyDir(dir) {
    pathInput.value = dir;
    if (!nameInput.dataset.userSet) {
      nameInput.value = getTabNameFromPath(dir);
    }
  }

  // Update name from URL when URL changes and name hasn't been manually set
  function updateNameFromUrl() {
    const url = urlInput.value.trim();
    if (!nameInput.dataset.userSet && url) {
      const derivedName = getTabNameFromUrl(url);
      if (derivedName) {
        nameInput.value = derivedName;
      }
    }
  }

  async function loadBrowser(path) {
    try {
      const url = path ? `/browse?path=${encodeURIComponent(path)}` : "/browse";
      const response = await fetch(url);
      const data = await response.json();
      store.browser.dir = data.current;
      store.browser.parent = data.parent;
      browserCurrent.textContent = data.current;
      upBtn.disabled = !data.parent;
      applyDir(data.current);
      browserEntries.textContent = "";
      if (data.entries.length === 0) {
        const empty = document.createElement("div");
        empty.className = "browser-empty muted";
        empty.textContent = "No subdirectories";
        browserEntries.appendChild(empty);
        return;
      }
      for (const entry of data.entries) {
        const btn = document.createElement("button");
        btn.type = "button";
        btn.className = "browser-entry";
        btn.textContent = `${entry.name}/`;
        btn.title = entry.path;
        btn.onclick = () => loadBrowser(entry.path);
        browserEntries.appendChild(btn);
      }
    } catch (err) {
      browserEntries.textContent = `Failed to list folders: ${err}`;
    }
  }

  upBtn.onclick = () => { if (store.browser.parent) loadBrowser(store.browser.parent); };
  homeBtn.onclick = () => loadBrowser("");

  // The browser is always open: seed it once per form appearance.
  if (!store.browser.dir && !store.browser.seeding) {
    store.browser.seeding = true;
    loadBrowser(pathInput.value.trim()).finally(() => {
      store.browser.seeding = false;
    });
  }

  pathInput.oninput = () => {
    if (!nameInput.dataset.userSet && pathInput.value) {
      nameInput.value = getTabNameFromPath(pathInput.value);
    }
  };

  urlInput.oninput = () => {
    updateNameFromUrl();
  };

  openBtn.onclick = () => {
    const path = pathInput.value.trim();
    const url = urlInput.value.trim();
    if (!path && !url) {
      errorEl.textContent = "Please select a directory or enter a URL";
      errorEl.style.display = "block";
      return;
    }
    errorEl.style.display = "none";
    store.awaitingNewTab = true;
    const payload = {
      name: nameInput.value.trim(),
      path,
    };
    if (url) {
      payload.url = url;
    }
    sendAction({ NewTab: JSON.stringify(payload) });
  };

  cancelBtn.onclick = () => {
    store.showAddForm = false;
    if (store.lastState && store.lastState.tabs.length > 0) {
      if (store.activeTabId === null || !store.lastState.tabs.some((t) => t.id === store.activeTabId)) {
        store.activeTabId = getInitialTabId(store.lastState);
      }
      updateUrlTab(store.activeTabId);
    } else {
      updateUrlTab(null);
    }
    render(store.lastState);
  };
}

//#endregion
// ======================================
// WebSocket Message Handling & Render
// ======================================
function handleStateMessage(event) {
  const state = JSON.parse(event.data);
  // Invariant: exactly one active view at all times — a real tab whenever
  // any exist, otherwise the client-local "+" form (activeTabId === null).
  if (state.tabs.length > 0) {
    if (store.activeTabId === null || !state.tabs.some((t) => t.id === store.activeTabId)) {
      store.activeTabId = getInitialTabId(state) ?? sortedTabs(state)[0].id;
    }
  } else {
    store.activeTabId = null;
  }
  if (store.lastState === null) {
    if (initialUrlView) {
      store.activeView = initialUrlView;
    }
  }
  const newIds = state.tabs.map((t) => t.id).filter((id) => !store.knownTabIds.has(id));
  if (store.awaitingNewTab) {
    const target = state.tabs.find((t) => newIds.includes(t.id));
    if (target) {
      store.activeTabId = target.id;
      store.awaitingNewTab = false;
      store.showAddForm = false;
    }
  }
  for (const id of state.tabs.map((t) => t.id)) {
    store.knownTabIds.add(id);
  }
  // Tab ids are never reused within a session, so a closed tab's entry can go
  // for good: a repo reopened later arrives under a fresh id and re-seeds.
  // Compared against the live tabs, not `knownTabIds` — that one accumulates
  // every id ever seen and would make this prune unreachable.
  for (const id of store.surfacedFailures.keys()) {
    if (!state.tabs.some((t) => t.id === id)) store.surfacedFailures.delete(id);
  }
  // A commit aimed at a tab that no longer exists can never resolve, and its
  // message would sit in the box looking unsent.
  if (store.pendingCommit && !state.tabs.some((t) => t.id === store.pendingCommit.tabId)) {
    store.pendingCommit = null;
  }
  store.lastState = state;
  if (state.revision === store.lastRevision) {
    return;
  }
  store.lastRevision = state.revision;
  store.pairCache.clear();
  store.commitCache.clear();
  updateUrlTab(store.activeTabId);
  render(state);
};
//#region WebSocket send + selection helpers

export function sendAction(action) {
  sendRaw(JSON.stringify({ tab: store.activeTabId, action }));
}

export function activeTab(state) {
  return state.tabs.find((t) => t.id === store.activeTabId) ?? state.tabs[0];
}

//#endregion
//#region Main render / view switching

export function render(state) {
  renderTabBar(state);

  const addRepoForm = document.getElementById("add-repo-form");
  const repoView = document.getElementById("repo-view");
  const historySection = document.getElementById("history-section");
  const logSection = document.getElementById("log-section");
  const actionsSection = document.getElementById("actions");
  const branchesSection = document.getElementById("branches-section");
  const stashesSection = document.getElementById("stashes-section");

  if (store.showAddForm || state.tabs.length === 0) {
    updateUrlTab(null);
    addRepoForm.style.display = "block";
    repoView.style.display = "none";
    historySection.style.display = "none";
    logSection.style.display = "none";
    actionsSection.style.display = "none";
    branchesSection.style.display = "none";
    stashesSection.style.display = "none";
    setDockVisible("files-section", false);
    setDockVisible("view-term-1", false);
    document.body.classList.remove("view-dashboard", "view-files", "view-term-1");
    setupAddRepoForm({ id: 0, repo_path: "" });
    document.title = "Grit | New Repository";
    return;
  }
  const tab = activeTab(state);

  addRepoForm.style.display = "none";
  document.title = `Grit | ${tab.name}`;
  showView(store.activeView);
  const overviewEl = document.getElementById("overview");
  const count = tab.state.changes.length;
  if (count === 0) {
    overviewEl.style.display = "none";
    overviewEl.textContent = "";
  } else {
    overviewEl.style.display = "";
    overviewEl.textContent =
      `${count} change${count === 1 ? "" : "s"}`;
  }

  renderScriptRunner(tab);
  renderBranches(tab);
  renderStashes(tab);
  renderLog(tab);
  renderActivityIndicators(tab);
  settlePendingCommit(state);
  renderFileBrowser(tab);

  const changesEl = document.getElementById("changes");
  let stillOpen = false;
  if (store.expanded.type === "file") store.expanded.el = null;
  changesEl.textContent = "";
  for (const change of tab.state.changes) {
    if (appendChangeRow(changesEl, change, tab)) stillOpen = true;
  }
  if (!stillOpen && store.expanded.type === "file") {
    store.expanded.type = null;
    store.expanded.key = null;
    store.expanded.el = null;
  }

  renderHistory(tab);
}

//#endregion
//#region History panel (recent commits + full-history search)

const RECENT_COMMIT_COUNT = 4;

// ======================================
// History List
// ======================================
function renderHistory(tab) {
  const historyEl = document.getElementById("history");
  historyEl.textContent = "";

  const needle = store.historyQuery.trim().toLowerCase();
  // When a search query is active the server has already replaced
  // tab.state.history with `git log --grep` results, so show all of them.
  // Without a query the state holds the default recent-commit window.
  const commits = needle
    ? tab.state.history
    : tab.state.history.slice(0, RECENT_COMMIT_COUNT);

  if (commits.length === 0) {
    const empty = document.createElement("div");
    empty.className = "muted";
    empty.textContent = needle ? "No matching commits." : "No commits yet.";
    historyEl.appendChild(empty);
    return;
  }

  let commitStillOpen = false;
  if (store.expanded.type === "commit") store.expanded.el = null;
  for (const commit of commits) {
    const key = `${tab.id}:${commit.hash}`;
    const row = document.createElement("div");
    row.className = "commit-row";

    const head = document.createElement("div");
    head.className = "commit-head";
    head.dataset.hash = commit.hash;

    const meta = document.createElement("span");
    meta.className = "commit-meta";
    meta.textContent = `${commit.hash.slice(0, 8)} ${commit.author}`;

    const msg = document.createElement("span");
    msg.className = "commit-msg";
    msg.textContent = commit.message;

    head.appendChild(meta);
    head.appendChild(msg);

    const actions = document.createElement("div");
    actions.className = "commit-actions";
    const expandedNow = store.expanded.type === "commit" && store.expanded.key === key;
    if (expandedNow) {
      commitStillOpen = true;
      store.expanded.el = actions;
    }
    actions.style.display = expandedNow ? "block" : "none";
    if (expandedNow) {
      buildCommitActions(actions, tab, commit.hash);
    }

    row.appendChild(head);
    row.appendChild(actions);
    historyEl.appendChild(row);
  }
  if (!commitStillOpen && store.expanded.type === "commit") {
    store.expanded.type = null;
    store.expanded.key = null;
    store.expanded.el = null;
  }
}

//#endregion
//#region Branches panel

// ======================================
// Branches
// ======================================
export function renderBranches(tab) {
  const branchListEl = document.getElementById("branch-list");
  branchListEl.textContent = "";
  const filter = (document.getElementById("branch-filter").value || "").trim().toLowerCase();
  const branches = tab.state.branches || [];
  const remoteBranches = tab.state.remote_branches || [];
  const current = tab.state.current_branch || "";

  document.getElementById("branch-subtitle").textContent = current;

  const filteredLocal = filter
    ? branches.filter((b) => b.toLowerCase().includes(filter))
    : branches;
  const filteredRemote = filter
    ? remoteBranches.filter((b) => b.toLowerCase().includes(filter))
    : remoteBranches;

  if (filteredLocal.length === 0 && filteredRemote.length === 0) {
    const empty = document.createElement("div");
    empty.className = "muted";
    empty.textContent = (branches.length === 0 && remoteBranches.length === 0)
      ? "No branches." : "No matching branches.";
    branchListEl.appendChild(empty);
    return;
  }

  function addBranchRow(branch, isRemote) {
    const row = document.createElement("div");
    row.className = "branch-row" + (branch === current ? " current" : "") + (isRemote ? " remote" : " local");

    const name = document.createElement("span");
    name.className = "branch-name";
    name.textContent = branch;
    row.appendChild(name);

    // For a remote-tracking ref (origin/xyz) the checkout must use the short
    // name so `git checkout xyz` DWIMs a local tracking branch instead of
    // leaving HEAD detached.
    const checkoutName = isRemote
      ? branch.split("/").slice(1).join("/")
      : branch;

    // The current branch can't be merged into itself, deleted while checked
    // out, or checked out again.
    const isCurrent = branch === current || checkoutName === current;

    const actions = document.createElement("div");
    actions.className = "branch-actions";

    const checkout = document.createElement("button");
    checkout.className = "branch-action-btn";
    checkout.dataset.branch = checkoutName;
    checkout.dataset.action = "checkout";
    checkout.textContent = "\u2192";
    checkout.title = `Switch to ${branch}`;
    actions.appendChild(checkout);

    const del = document.createElement("button");
    del.className = "branch-action-btn branch-delete";
    del.dataset.branch = branch;
    del.dataset.action = "delete";
    del.textContent = "\u00d7";
    del.title = `Delete branch ${branch}`;
    actions.appendChild(del);

    const merge = document.createElement("button");
    merge.className = "branch-action-btn branch-merge";
    merge.dataset.branch = branch;
    merge.dataset.action = "merge";
    merge.textContent = "\u2294";
    merge.title = `Merge ${branch} into ${current} and push`;
    actions.appendChild(merge);

    // The current branch can't be merged into itself, deleted while checked
    // out, or checked out again — grey those buttons out.
    if (isCurrent) {
      checkout.disabled = true;
      del.disabled = true;
      merge.disabled = true;
      merge.title = `${branch} is the current branch — nothing to merge into`;
    }

    if (isCurrent) {
      const label = document.createElement("span");
      label.className = "branch-current-label";
      label.textContent = "current";
      row.appendChild(label);
    }

    row.appendChild(actions);

    branchListEl.appendChild(row);
  }

  for (const branch of filteredLocal) {
    addBranchRow(branch, false);
  }

  if (filteredRemote.length > 0) {
    for (const branch of filteredRemote) {
      addBranchRow(branch, true);
    }
  }
}

//#endregion
//#region Stashes panel

// ======================================
// Stashes
// ======================================
export function renderStashes(tab) {
  const listEl = document.getElementById("stash-list");
  listEl.textContent = "";
  const stashes = tab.state.stashes || [];
  const subtitleEl = document.getElementById("stash-subtitle");
  if (stashes.length === 0) {
    subtitleEl.style.display = "none";
    subtitleEl.textContent = "";
  } else {
    subtitleEl.style.display = "";
    subtitleEl.textContent =
      stashes.length === 1 ? "1 stash" : `${stashes.length} stashes`;
  }

  if (stashes.length === 0) {
    return;
  }

  let stillOpen = false;
  if (store.expanded.type === "stash") store.expanded.el = null;
  for (const stash of stashes) {
    const key = `${tab.id}:${stash.id}`;
    const row = document.createElement("div");
    row.className = "stash-row";

    const head = document.createElement("div");
    head.className = "stash-head";
    head.dataset.id = stash.id;

    const meta = document.createElement("span");
    meta.className = "stash-meta muted";
    meta.textContent = `${stash.id} \u00b7 ${stash.branch}`;

    const msg = document.createElement("span");
    msg.className = "stash-msg";
    msg.textContent = stash.message || "(no message)";

    head.appendChild(meta);
    head.appendChild(msg);

    const actions = document.createElement("div");
    actions.className = "stash-actions";
    const expandedNow = store.expanded.type === "stash" && store.expanded.key === key;
    if (expandedNow) {
      stillOpen = true;
      store.expanded.el = actions;
      actions.style.display = "block";
      buildStashActions(actions, stash);
    } else {
      actions.style.display = "none";
    }

    row.appendChild(head);
    row.appendChild(actions);
    listEl.appendChild(row);
  }
  if (!stillOpen && store.expanded.type === "stash") {
    store.expanded.type = null;
    store.expanded.key = null;
    store.expanded.el = null;
  }
}

function buildStashActions(actionsEl, stash) {
  actionsEl.textContent = "";
  const fileList = document.createElement("div");
  fileList.className = "stash-files";
  if (stash.files && stash.files.length > 0) {
    for (const file of stash.files) {
      const row = document.createElement("div");
      row.className = "stash-file";
      const counts = lineCounts(file.insertions, file.deletions);
      row.textContent = counts
        ? `${file.status} ${file.path} ${counts}`
        : `${file.status} ${file.path}`;
      fileList.appendChild(row);
    }
  } else {
    const empty = document.createElement("div");
    empty.className = "muted";
    empty.textContent = "No files in this stash.";
    fileList.appendChild(empty);
  }
  actionsEl.appendChild(fileList);

  const btnRow = document.createElement("div");
  btnRow.className = "stash-btn-row";
  const defs = [
    { label: "Apply", run: () => sendAction({ StashApply: stash.id }) },
    { label: "Pop", run: () => sendAction({ StashPop: stash.id }) },
    { label: "Drop", run: () => sendAction({ StashDrop: stash.id }) },
  ];
  for (const def of defs) {
    const btn = document.createElement("button");
    btn.textContent = def.label;
    btn.onclick = def.run;
    btnRow.appendChild(btn);
  }
  actionsEl.appendChild(btnRow);
}

//#endregion
//#region Script runner (Project Actions)

// ======================================
// Script Runner
// ======================================
function renderScriptRunner(tab) {
  const scripts = tab.state.scripts || [];
  const select = document.getElementById("script-select");
  const runBtn = document.getElementById("run-script-btn");
  if (scripts.length === 0) {
    select.style.display = "none";
    runBtn.style.display = "none";
    select.textContent = "";
    return;
  }

  const previous = select.value;
  select.textContent = "";
  for (const script of scripts) {
    const option = document.createElement("option");
    option.value = script.rel_path;
    option.textContent = script.rel_path;
    select.appendChild(option);
  }
  // Keep the user's selection across broadcasts when it still exists.
  if (scripts.some((s) => s.rel_path === previous)) {
    select.value = previous;
  }
  select.style.display = "";
  runBtn.style.display = "";
}

//#endregion
//#region Folio file browser (embedded external file explorer)

// Path of the active repo ("" when no tab is selected). Used by the folio and
// krust iframe URL builders, so it lives with the core.
export function currentRepoPath() {
  if (!store.lastState || store.lastState.tabs.length === 0) return "";
  const tab = activeTab(store.lastState);
  return (tab && tab.repo_path) ? tab.repo_path : "";
}

function folioFrameSrc() {
  return `${FOLIO_BASE}/?dir=${encodeURIComponent(currentRepoPath())}`;
}

export function ensureFolioFrame() {
  const frame = document.getElementById("folio-frame");
  if (!frame) return;
  const target = folioFrameSrc();
  if (frame.getAttribute("src") === target) return;
  frame.setAttribute("src", target);
}

function renderFileBrowser(tab) {
  setDockVisible("files-section", store.activeView === "files");
  ensureFolioFrame();
}

//#endregion
//#region Command log (terminal-style transcript per tab)

// ======================================
// Log
// ======================================
function renderLog(tab) {
  const logEl = document.getElementById("log");
  // Stick to the bottom while streaming, unless the user scrolled up.
  const nearBottom =
    logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < SCROLL_HIT_SLACK_PX;

  const entries = (tab.log || []).filter((e) => e.seq > store.clearedUpToSeq);
  if (entries.length === 0) {
    logEl.textContent = "";
    const empty = document.createElement("div");
    empty.className = "log-empty muted";
    empty.textContent = "No commands yet.";
    logEl.appendChild(empty);
    return;
  }

  logEl.textContent = "";
  for (const entry of entries) {
    const row = document.createElement("div");
    row.className = `log-entry ${entry.status}`;

    const head = document.createElement("div");
    head.className = "log-cmd";

    const cmd = document.createElement("span");
    cmd.className = "log-cmd-text";
    cmd.textContent = entry.command;
    head.appendChild(cmd);

    if (entry.duration_ms > 0) {
      const dur = document.createElement("span");
      dur.className = "log-duration";
      dur.textContent = `${(entry.duration_ms / 1000).toFixed(1)}s`;
      head.appendChild(dur);
    }

    row.appendChild(head);

    if (entry.output) {
      const out = document.createElement("pre");
      out.className = "log-out";
      out.textContent = entry.output;
      row.appendChild(out);
    }

    logEl.appendChild(row);
  }
  if (nearBottom) {
    logEl.scrollTop = logEl.scrollHeight;
  }
}

// Drives the spinner/red cross in the Actions and Changes rollup bars and, on
// failure, reveals the transcript. One shared state rather than one per rollup:
// every button on the dashboard — the commit bar and per-file stage controls
// included — feeds the same transcript, so a spinner that appeared only in the
// rollup a button did not live in would be feedback the user has to look away
// to find. The newest log entry is authoritative: a `running` placeholder is
// always appended last and revised in place while the action runs, then
// `finish_log_entry` replaces it with fresh seqs once it completes.
function renderActivityIndicators(tab) {
  const indicators = document.querySelectorAll("[data-activity]");
  let newest = null;
  for (const entry of tab.log || []) {
    if (!newest || entry.seq > newest.seq) newest = entry;
  }

  if (!store.surfacedFailures.has(tab.id)) {
    // First frame for this tab: adopt whatever it already has silently, so
    // failures from before this page load do not hijack the viewport.
    store.surfacedFailures.set(tab.id, newest ? newest.seq : 0);
  }

  for (const el of indicators) {
    el.classList.remove("running", "failed");
    if (!newest) {
      el.hidden = true;
      continue;
    }
    el.hidden = false;
    el.title = newest.command;
    if (newest.status === "running") {
      el.classList.add("running");
    } else if (newest.status === "failed") {
      el.classList.add("failed");
      el.title = `${newest.command} failed — see the Log panel`;
    }
  }

  // Decided once, outside the loop: with two indicators an in-loop guard would
  // consume the failure on the first element and leave the second guessing.
  if (newest && newest.status === "failed" && newest.seq > store.surfacedFailures.get(tab.id)) {
    store.surfacedFailures.set(tab.id, newest.seq);
    revealFailedCommand();
  }
}

// Unfolds the Log rollup and scrolls the failing command into view, so a failed
// action cannot pass unnoticed. Reuses the same collapse class and ARIA pairing
// as the click handler on `.section-title`.
function revealFailedCommand() {
  const section = document.getElementById("log-section");
  const title = section.querySelector(".section-title");
  if (section.classList.contains("collapsed")) {
    section.classList.remove("collapsed");
    title.setAttribute("aria-expanded", "true");
  }
  // The transcript only exists on the dashboard. Every action button lives
  // there too, but one can still be in flight when the user switches views.
  if (section.style.display === "none") {
    showView("dashboard");
  }
  // Must follow renderLog, which rebuilds the entries this scrolls.
  const logEl = document.getElementById("log");
  logEl.scrollTop = logEl.scrollHeight;
  section.scrollIntoView({ block: "nearest" });
}

// Reads the commit outcome out of the transcript entries that landed after the
// click: "success", "failed", or null while the action is still in flight.
//
// `commitCommand` is the exact line `action_argv` will produce for
// `["commit", "-m", message]` — `format_argv` does not quote its arguments, so
// the comparison is a plain string equality. Matching exactly rather than on a
// `git commit ` prefix is what keeps two rapid clicks from crediting the second
// message with the first action's verdict.
//
// `running` entries are skipped so the `&&`-joined preview placeholder — which
// is the whole line, `git commit -m ...` for a plain Commit — is never mistaken
// for a verdict. Any earlier failure is terminal too: `add -A` is the only step
// before the commit, so if it failed there is nothing to clear.
function commitOutcome(entries, commitCommand) {
  for (const entry of entries) {
    if (entry.status === "running") continue;
    if (entry.command === commitCommand) {
      return entry.status === "success" ? "success" : "failed";
    }
    if (entry.status === "failed") return "failed";
  }
  return null;
}

// Clears the commit box only once the commit itself landed. Resolved against
// every tab, not just the active one, so a commit that finishes while another
// tab is selected still clears the box when it lands.
function settlePendingCommit(state) {
  if (!store.pendingCommit) return;
  const tab = state.tabs.find((t) => t.id === store.pendingCommit.tabId);
  if (!tab) return;
  const entries = (tab.log || []).filter((e) => e.seq > store.pendingCommit.afterSeq);
  const outcome = commitOutcome(entries, `git commit -m ${store.pendingCommit.message}`);
  if (!outcome) return;
  const message = store.pendingCommit.message;
  store.pendingCommit = null;
  // Only ever clear what we sent: the user may have typed a new message, or
  // switched tabs and started typing in this box, while git was working.
  if (outcome === "success" && commitMsg.value.trim() === message) {
    commitMsg.value = "";
  }
}

//#endregion
//#region Changes + commit rendering

// ======================================
// Staging & Commit Summary
// ======================================
function appendChangeRow(container, change, tab) {
  const key = `${tab.id}:${change.path}`;
  const row = document.createElement("div");
  row.className = "change-row";

  const head = document.createElement("div");
  head.className = "change-head";
  head.dataset.path = change.path;

  const status = document.createElement("span");
  status.className = "change-status";
  status.textContent = change.status;
  if (change.is_staged) {
    status.classList.add("staged");
  }

  const file = document.createElement("span");
  file.className = "change-file";
  file.textContent = change.path;

  const actions = document.createElement("div");
  actions.className = "change-actions";

  const navDown = document.createElement("button");
  navDown.className = "action-btn block-nav";
  navDown.dataset.action = "next-block";
  navDown.textContent = "v";
  navDown.title = "Scroll to next change block";

  const navUp = document.createElement("button");
  navUp.className = "action-btn block-nav";
  navUp.dataset.action = "prev-block";
  navUp.textContent = "^";
  navUp.title = "Scroll to previous change block";

  actions.appendChild(navDown);
  actions.appendChild(navUp);

  const action = document.createElement("button");
  action.className = "action-btn";
  action.dataset.path = change.path;
  action.dataset.action = "stage";
  action.dataset.staged = change.is_staged ? "true" : "false";
  action.textContent = change.is_staged ? "−" : "+";
  action.title = change.is_staged ? "Unstage file" : "Stage file";

  const discard = document.createElement("button");
  discard.className = "action-btn discard-btn";
  discard.dataset.path = change.path;
  discard.dataset.status = change.status;
  discard.dataset.action = "discard";
  discard.textContent = "×";
  discard.title = "Discard changes to this file";

  actions.appendChild(action);
  actions.appendChild(discard);

  head.appendChild(status);
  head.appendChild(file);
  head.appendChild(actions);

  const detail = document.createElement("div");
  detail.className = "change-diff";
  const expandedNow = store.expanded.type === "file" && store.expanded.key === key;
  if (expandedNow) {
    store.expanded.el = detail;
  }
  detail.style.display = expandedNow ? "block" : "none";
  if (expandedNow) {
    showDiff(detail, tab, change.path);
  }

  row.appendChild(head);
  row.appendChild(detail);
  if (expandedNow) {
    row.classList.add("open");
  }
  container.appendChild(row);
  return expandedNow;
}

function toggleCommitActions(actionsEl, tab, hash) {
  const key = `${tab.id}:${hash}`;
  if (toggleSection("commit", key, actionsEl)) {
    actionsEl.style.display = "block";
    buildCommitActions(actionsEl, tab, hash);
  } else {
    actionsEl.style.display = "none";
  }
}

function buildCommitActions(actionsEl, tab, hash) {
  actionsEl.textContent = "";
  const defs = [
    {
      label: "Revert",
      run: () => sendAction({ Revert: hash }),
    },
    {
      label: "Branch",
      run: () => {
        const name = prompt("New branch name:");
        if (name) sendAction({ CreateBranch: [name, hash] });
      },
    },
    {
      label: "Checkout",
      run: () => sendAction({ CheckoutBranch: hash }),
    },
    {
      label: "Tag",
      run: () => {
        const name = prompt("New tag name:");
        if (name) sendAction({ CreateTag: [name, hash] });
      },
    },
    {
      label: "Delete Tag",
      run: () => {
        const name = prompt("Tag name to delete:");
        if (name) sendAction({ DeleteTag: name });
      },
    },
    {
      label: "Delete Branch",
      run: () => {
        const name = prompt("Branch name to delete:");
        if (name) sendAction({ DeleteBranch: { name, local: true, remote: false } });
      },
    },
  ];
  for (const def of defs) {
    const btn = document.createElement("button");
    btn.textContent = def.label;
    btn.onclick = def.run;
    actionsEl.appendChild(btn);
  }
  showCommitSummary(actionsEl, tab, hash);
}

async function showCommitSummary(actionsEl, tab, hash) {
  const key = `${tab.id}:${hash}`;
  const cached = store.commitCache.get(key);
  if (cached) {
    actionsEl.appendChild(renderCommitSummary(cached));
    return;
  }
  const placeholder = document.createElement("div");
  placeholder.className = "commit-summary";
  placeholder.textContent = "Loading summary...";
  actionsEl.appendChild(placeholder);
  try {
    const response = await fetch(
      `/commit?tab=${tab.id}&hash=${encodeURIComponent(hash)}`
    );
    if (!response.ok) {
      throw new Error(`HTTP ${response.status}`);
    }
    const summary = await response.json();
    store.commitCache.set(key, summary);
    placeholder.replaceWith(renderCommitSummary(summary));
  } catch (err) {
    placeholder.textContent = `Failed to load summary: ${err}`;
  }
}

function renderCommitSummary(summary) {
  const div = document.createElement("div");
  div.className = "commit-summary";

  const date = new Date(summary.timestamp * 1000).toLocaleString();

  const msg = document.createElement("div");
  msg.className = "commit-summary-msg";
  msg.textContent = summary.message;

  const meta = document.createElement("div");
  meta.className = "commit-summary-meta";
  meta.textContent = `${summary.author} \u2022 ${date}`;

  const stat = document.createElement("div");
  stat.className = "commit-summary-stat";
  stat.textContent = `${summary.files_changed} files changed, ${lineCounts(
    summary.insertions,
    summary.deletions
  )}`;

  div.appendChild(msg);
  div.appendChild(meta);
  div.appendChild(stat);

  for (const file of summary.files) {
    const row = document.createElement("div");
    row.className = "commit-summary-file";
    const counts = lineCounts(file.insertions, file.deletions);
    row.textContent = counts
      ? `${file.status} ${file.path} ${counts}`
      : `${file.status} ${file.path}`;
    div.appendChild(row);
  }
  return div;
}

//#endregion
//#region Tab bar, sorting, URL deep-linking

// ======================================
// Tab Bar
// ======================================
export function renderTabBar(state) {
  const tabsEl = document.getElementById("tabs");
  tabsEl.textContent = "";
  for (const tab of sortedTabs(state)) {
    const group = document.createElement("div");
    group.className = "tab-group";

    const nameBtn = document.createElement("button");
    nameBtn.className = "tab-name";
    if (tab.id === store.activeTabId && store.activeView === "dashboard") {
      nameBtn.classList.add("active");
    }
    if (tab.state.changes.length > 0) {
      nameBtn.classList.add("dirty");
    }
    nameBtn.textContent = tab.name;
    nameBtn.title = tab.name;
    // Remote is ahead of the last fetch. Non-interactive: the Pull button is
    // the action, this is only a hint that one is warranted.
    if (tab.state.out_of_date) {
      const badge = document.createElement("span");
      badge.className = "tab-badge tab-badge-sync";
      badge.textContent = "↑";
      badge.title = "Remote has commits you do not have — pull";
      nameBtn.appendChild(badge);
      nameBtn.title = `${tab.name} — remote has new commits`;
    }
    nameBtn.dataset.tabId = tab.id;
    nameBtn.dataset.view = "dashboard";
    group.appendChild(nameBtn);

    const filesBtn = document.createElement("button");
    filesBtn.className = "tab-view-btn";
    filesBtn.classList.add("folio-btn");
    filesBtn.style.display = store.folioAvailable ? "" : "none";
    if (tab.id === store.activeTabId && store.activeView === "files") {
      filesBtn.classList.add("active");
    }
    filesBtn.textContent = "F";
    filesBtn.title = "Files";
    filesBtn.dataset.tabId = tab.id;
    filesBtn.dataset.view = "files";
    group.appendChild(filesBtn);

    const termBtn = document.createElement("button");
    termBtn.className = "tab-view-btn";
    termBtn.classList.add("krust-btn");
    termBtn.style.display = store.krustAvailable ? "" : "none";
    if (tab.id === store.activeTabId && store.activeView === "term-1") {
      termBtn.classList.add("active");
    }
    termBtn.textContent = "T";
    termBtn.title = "Terminal";
    termBtn.dataset.tabId = tab.id;
    termBtn.dataset.view = "term-1";
    // Unseen-output dot: only the session currently bound to the krust
    // iframe can accumulate output while it is off screen, and the ping
    // names that session — see handleKrustActivity.
    if (store.termUnseenSid !== null && store.termUnseenSid === sessionIdForRepo(tab.repo_path || "", KRUST_SESSIONS["term-1"])) {
      const badge = document.createElement("span");
      badge.className = "tab-badge tab-badge-term";
      badge.title = "New terminal output since you last looked";
      termBtn.appendChild(badge);
    }
    group.appendChild(termBtn);

    tabsEl.appendChild(group);
  }
  const newTabBtn = document.createElement("button");
  newTabBtn.className = "tab new-tab";
  newTabBtn.textContent = "+";
  newTabBtn.title = "New tab";
  tabsEl.appendChild(newTabBtn);
}

function sortedTabs(state) {
  return [...state.tabs].sort((a, b) =>
    a.name.localeCompare(b.name, undefined, { sensitivity: "base" })
  );
}

export function updateUrlTab(tabId) {
  const url = new URL(window.location.href);
  const tabs = store.lastState ? sortedTabs(store.lastState) : [];
  const idx = tabId === null ? -1 : tabs.findIndex((t) => t.id === tabId);
  if (idx >= 0) {
    url.searchParams.set("t", String(idx));
  } else {
    url.searchParams.delete("t");
  }
  window.history.replaceState({}, "", url);
}

function getInitialTabId(state) {
  const url = new URL(window.location.href);
  const t = url.searchParams.get("t");
  const sorted = sortedTabs(state);
  if (t !== null) {
    const idx = parseInt(t, 10);
    if (!isNaN(idx) && idx >= 0 && idx < sorted.length) {
      return sorted[idx].id;
    }
  }
  // state.active is an index into the tab list, not an id.
  const byIndex = sorted[state.active] ?? sorted[0];
  return byIndex ? byIndex.id : null;
}

//#endregion
//#region View dock (Dashboard / Files / Log / Terminals)

// Dock views hold cross-origin iframes and are never taken out of the layout —
// see the `.term-view` note in style.css. Visibility is therefore a class
// toggle, not a `display` one: the embedded document keeps its box, its
// animation frames and its renderer while it is off screen.
export function setDockVisible(id, visible) {
  document.getElementById(id).classList.toggle("is-active", visible);
}

//#endregion

export const commitMsg = document.getElementById("commit-msg");

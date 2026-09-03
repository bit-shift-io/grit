let ws = null;
let reconnectTimer = null;
let reconnectDelayMs = 500;

function scheduleReconnect(delay) {
  if (reconnectTimer !== null) return;
  reconnectTimer = setTimeout(() => {
    reconnectTimer = null;
    openSocket();
  }, delay);
}

function openSocket() {
  const socket = new WebSocket(`ws://${location.host}/ws`);
  ws = socket;
  socket.onopen = () => {
    reconnectDelayMs = 500;
    setConnStatus(true);
  };
  socket.onmessage = handleStateMessage;
  socket.onclose = () => {
    if (ws === socket) setConnStatus(false);
    scheduleReconnect(reconnectDelayMs);
    reconnectDelayMs = Math.min(reconnectDelayMs * 2, 5000);
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

function sendRaw(payload) {
  if (!ws || ws.readyState !== WebSocket.OPEN) return;
  ws.send(payload);
}

document.addEventListener("visibilitychange", () => {
  if (document.hidden) return;
  if (ws && (ws.readyState === WebSocket.OPEN || ws.readyState === WebSocket.CONNECTING)) return;
  if (reconnectTimer !== null) {
    clearTimeout(reconnectTimer);
    reconnectTimer = null;
  }
  openSocket();
});

window.addEventListener("pageshow", (event) => {
  if (!event.persisted) return;
  if (ws && ws.readyState !== WebSocket.CLOSED) return;
  openSocket();
});

openSocket();

let activeTabId = null;
let lastState = null;
let expandedKey = null;
let expandedDetailEl = null;
let expandedCommitKey = null;
let expandedCommitEl = null;
let awaitingNewTab = false;
// Client-local view state: entries with seq <= this are hidden by "Clear Log".
let clearedUpToSeq = 0;
// Local-only view state: the "+" form never exists as a server-side tab.
let showAddForm = false;
let browserDir = null;
let browserParent = null;
let browserSeeding = false;
let historyQuery = "";
let historySearchTimer = null;
const knownTabIds = new Set();
const commitCache = new Map();
const pairCache = new Map();

//#region Add-repo form ("+" is a client-local mode, never a server tab)

function getTabNameFromPath(path) {
  const parts = path.trim().split(/[/\\]/).filter(p => p.length > 0);
  const name = parts[parts.length - 1] || "repo";
  return name.replace(/-/g, " ");
}

function setupAddRepoForm(tab) {
  const nameInput = document.getElementById("new-repo-name");
  const pathInput = document.getElementById("new-repo-path");
  const errorEl = document.getElementById("new-repo-error");

  if (nameInput.dataset.tabId !== String(tab.id)) {
    nameInput.dataset.tabId = String(tab.id);
    nameInput.value = "";
    pathInput.value = "";
    delete nameInput.dataset.userSet;
    errorEl.style.display = "none";
    browserDir = null;
    document.getElementById("folder-browser").style.display = "block";
  }

  const browserEl = document.getElementById("folder-browser");
  const browserCurrent = document.getElementById("browser-current");
  const browserEntries = document.getElementById("browser-entries");
  const upBtn = document.getElementById("browser-up-btn");
  const homeBtn = document.getElementById("browser-home-btn");
  const openBtn = document.getElementById("open-repo-btn");
  const cancelBtn = document.getElementById("cancel-repo-btn");

  nameInput.oninput = () => { nameInput.dataset.userSet = "true"; };

  // The path and name fields follow the browser's current folder so the
  // user can just hit "Open Repository" without a separate select step.
  function applyDir(dir) {
    pathInput.value = dir;
    if (!nameInput.dataset.userSet) {
      nameInput.value = getTabNameFromPath(dir);
    }
  }

  async function loadBrowser(path) {
    try {
      const url = path ? `/browse?path=${encodeURIComponent(path)}` : "/browse";
      const response = await fetch(url);
      const data = await response.json();
      browserDir = data.current;
      browserParent = data.parent;
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

  upBtn.onclick = () => { if (browserParent) loadBrowser(browserParent); };
  homeBtn.onclick = () => loadBrowser("");

  // The browser is always open: seed it once per form appearance.
  if (!browserDir && !browserSeeding) {
    browserSeeding = true;
    loadBrowser(pathInput.value.trim()).finally(() => {
      browserSeeding = false;
    });
  }

  pathInput.oninput = () => {
    if (!nameInput.dataset.userSet && pathInput.value) {
      nameInput.value = getTabNameFromPath(pathInput.value);
    }
  };

  openBtn.onclick = () => {
    const path = pathInput.value.trim();
    if (!path) {
      errorEl.textContent = "Please select a directory";
      errorEl.style.display = "block";
      return;
    }
    errorEl.style.display = "none";
    awaitingNewTab = true;
    sendAction({ NewTab: JSON.stringify({ name: nameInput.value.trim(), path }) });
  };

  cancelBtn.onclick = () => {
    showAddForm = false;
    if (lastState && lastState.tabs.length > 0) {
      if (activeTabId === null || !lastState.tabs.some((t) => t.id === activeTabId)) {
        activeTabId = getInitialTabId(lastState);
      }
      updateUrlTab(activeTabId);
    } else {
      updateUrlTab(null);
    }
    render(lastState);
  };
}

function handleStateMessage(event) {
  const state = JSON.parse(event.data);
  // Invariant: exactly one active view at all times — a real tab whenever
  // any exist, otherwise the client-local "+" form (activeTabId === null).
  if (state.tabs.length > 0) {
    if (activeTabId === null || !state.tabs.some((t) => t.id === activeTabId)) {
      activeTabId = getInitialTabId(state) ?? sortedTabs(state)[0].id;
    }
  } else {
    activeTabId = null;
  }
  const newIds = state.tabs.map((t) => t.id).filter((id) => !knownTabIds.has(id));
  if (awaitingNewTab) {
    const target = state.tabs.find((t) => newIds.includes(t.id));
    if (target) {
      activeTabId = target.id;
      awaitingNewTab = false;
      showAddForm = false;
    }
  }
  for (const id of state.tabs.map((t) => t.id)) {
    knownTabIds.add(id);
  }
  const prev = lastState;
  lastState = state;
  if (prev !== null && JSON.stringify(prev) === JSON.stringify(state)) {
    return;
  }
  pairCache.clear();
  commitCache.clear();
  updateUrlTab(activeTabId);
  render(state);
};

//#endregion

//#region WebSocket send + selection helpers

function sendAction(action) {
  sendRaw(JSON.stringify({ tab: activeTabId, action }));
}

function activeTab(state) {
  return state.tabs.find((t) => t.id === activeTabId) ?? state.tabs[0];
}

//#endregion

//#region Main render / view switching

function render(state) {
  renderTabBar(state);

  const addRepoForm = document.getElementById("add-repo-form");
  const repoView = document.getElementById("repo-view");
  const historySection = document.getElementById("history-section");
  const logSection = document.getElementById("log-section");
  const actionsSection = document.getElementById("actions");
  const branchesSection = document.getElementById("branches-section");

  if (showAddForm || state.tabs.length === 0) {
    updateUrlTab(null);
    addRepoForm.style.display = "block";
    repoView.style.display = "none";
    historySection.style.display = "none";
    logSection.style.display = "none";
    actionsSection.style.display = "none";
    branchesSection.style.display = "none";
    setupAddRepoForm({ id: 0, repo_path: "" });
    document.title = "Grit | New Repository";
    return;
  }
  const tab = activeTab(state);
  
  addRepoForm.style.display = "none";
  repoView.style.display = "block";
  historySection.style.display = "block";
  logSection.style.display = "block";
  actionsSection.style.display = "block";
  branchesSection.style.display = "block";
  document.title = `Grit | ${tab.name}`;
  document.getElementById("overview").textContent =
    `${tab.state.current_branch} — ${tab.state.changes.length} change(s)`;

  renderScriptRunner(tab);
  renderBranches(tab);
  renderLog(tab);

  const changesEl = document.getElementById("changes");
  let stillOpen = false;
  expandedDetailEl = null;
  changesEl.textContent = "";
  for (const change of tab.state.changes) {
    if (appendChangeRow(changesEl, change, tab)) stillOpen = true;
  }
  if (!stillOpen) {
    expandedKey = null;
    expandedDetailEl = null;
  }

  renderHistory(tab);
}

//#endregion

//#region History panel (recent commits + full-history search)

const RECENT_COMMIT_COUNT = 4;

function renderHistory(tab) {
  const historyEl = document.getElementById("history");
  historyEl.textContent = "";

  const needle = historyQuery.trim().toLowerCase();
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
  expandedCommitEl = null;
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
    const expanded = expandedCommitKey === key;
    if (expanded) {
      commitStillOpen = true;
      expandedCommitEl = actions;
    }
    actions.style.display = expanded ? "block" : "none";
    if (expanded) {
      buildCommitActions(actions, tab, commit.hash);
    }

    row.appendChild(head);
    row.appendChild(actions);
    historyEl.appendChild(row);
  }
  if (!commitStillOpen) {
    expandedCommitKey = null;
    expandedCommitEl = null;
  }
}

//#endregion

//#region Branches panel

function renderBranches(tab) {
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

    if (branch === current || checkoutName === current) {
      const label = document.createElement("span");
      label.className = "branch-current-label";
      label.textContent = "current";
      row.appendChild(label);
    } else {
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

      row.appendChild(actions);
    }

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

//#region Script runner (Project Actions)

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

document.getElementById("run-script-btn").onclick = () => {
  const relPath = document.getElementById("script-select").value;
  if (!relPath) return;
  sendAction({ RunScript: relPath });
};

//#endregion

//#region Command log (terminal-style transcript per tab)

function renderLog(tab) {
  const logEl = document.getElementById("log");
  // Stick to the bottom while streaming, unless the user scrolled up.
  const nearBottom =
    logEl.scrollHeight - logEl.scrollTop - logEl.clientHeight < 48;

  const entries = (tab.log || []).filter((e) => e.seq > clearedUpToSeq);
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

document.getElementById("clear-log-btn").onclick = () => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  const maxSeq = Math.max(0, ...(tab.log || []).map((e) => e.seq));
  clearedUpToSeq = maxSeq;
  render(lastState);
};

//#endregion

//#region Changes + commit rendering

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
  const expanded = expandedKey === key;
  if (expanded) {
    expandedDetailEl = detail;
  }
  detail.style.display = expanded ? "block" : "none";
  if (expanded) {
    showDiff(detail, tab, change.path);
  }

  row.appendChild(head);
  row.appendChild(detail);
  if (expanded) {
    row.classList.add("open");
  }
  container.appendChild(row);
  return expanded;
}

function toggleCommitActions(actionsEl, tab, hash) {
  const key = `${tab.id}:${hash}`;
  if (expandedCommitKey === key) {
    expandedCommitKey = null;
    expandedCommitEl.style.display = "none";
    expandedCommitEl = null;
    return;
  }
  if (expandedCommitEl) {
    expandedCommitEl.style.display = "none";
  }
  expandedCommitKey = key;
  expandedCommitEl = actionsEl;
  actionsEl.style.display = "block";
  buildCommitActions(actionsEl, tab, hash);
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
        if (name) sendAction({ DeleteBranch: name });
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
  const cached = commitCache.get(key);
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
    commitCache.set(key, summary);
    placeholder.replaceWith(renderCommitSummary(summary));
  } catch (err) {
    placeholder.textContent = `Failed to load summary: ${err}`;
  }
}

function lineCounts(ins, del) {
  const parts = [];
  if (ins > 0) parts.push(`+${ins}`);
  if (del > 0) parts.push(`-${del}`);
  return parts.join("/");
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

function renderTabBar(state) {
  const tabsEl = document.getElementById("tabs");
  tabsEl.textContent = "";
  for (const tab of sortedTabs(state)) {
    const btn = document.createElement("button");
    btn.className = "tab";
    if (tab.id === activeTabId) {
      btn.classList.add("active");
    }
    btn.textContent = tab.name;
    if (tab.state.changes.length > 0) {
      btn.classList.add("dirty");
    }
    btn.dataset.tabId = tab.id;
    tabsEl.appendChild(btn);
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

function updateUrlTab(tabId) {
  const url = new URL(window.location.href);
  const tabs = lastState ? sortedTabs(lastState) : [];
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

async function toggleDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  if (expandedKey === key) {
    expandedKey = null;
    expandedDetailEl.style.display = "none";
    expandedDetailEl.textContent = "";
    expandedDetailEl = null;
    detailEl.closest(".change-row").classList.remove("open");
    return;
  }
  if (expandedDetailEl) {
    expandedDetailEl.style.display = "none";
    expandedDetailEl.textContent = "";
    expandedDetailEl.closest(".change-row").classList.remove("open");
  }
  expandedKey = key;
  expandedDetailEl = detailEl;
  detailEl.style.display = "block";
  detailEl.closest(".change-row").classList.add("open");
  await showDiff(detailEl, tab, path);
  if (expandedDetailEl === detailEl) {
    scrollToFirstDiffBlock(detailEl);
  }
}

function scrollToFirstDiffBlock(detailEl) {
  const first = detailEl.querySelector("tbody.diff-block");
  if (first) {
    scrollDetailToBlock(detailEl, first);
  }
}

// Scrolls only the diff's own scroll box (never the page), centering the
// target block vertically.
function scrollDetailToBlock(detailEl, block) {
  const dRect = detailEl.getBoundingClientRect();
  const bRect = block.getBoundingClientRect();
  const delta =
    bRect.top - dRect.top - (dRect.height - bRect.height) / 2;
  detailEl.scrollBy({ top: delta, behavior: "smooth" });
}

// Steps between contiguous changed-row groups ("blocks") in an expanded diff.
// The current block is derived from the scroll position so manual scrolling
// never desyncs the ^/v buttons.
function stepDiffBlock(headEl, delta) {
  const detail = headEl ? headEl.nextElementSibling : null;
  if (!detail || detail.style.display === "none") return;
  const blocks = detail.querySelectorAll("tbody.diff-block");
  if (blocks.length === 0) return;
  const marker = detail.getBoundingClientRect().top + detail.clientHeight * 0.35;
  let index = 0;
  for (let i = 0; i < blocks.length; i++) {
    if (blocks[i].getBoundingClientRect().top <= marker) index = i;
  }
  const target = Math.max(0, Math.min(index + delta, blocks.length - 1));
  scrollDetailToBlock(detail, blocks[target]);
}

async function showDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  const cached = pairCache.get(key);
  if (cached) {
    renderFilePair(detailEl, cached);
    return;
  }
  detailEl.textContent = "Loading...";
  try {
    const response = await fetch(`/files?tab=${tab.id}&path=${encodeURIComponent(path)}`);
    const pair = await response.json();
    pairCache.set(key, pair);
    renderFilePair(detailEl, pair);
  } catch (err) {
    detailEl.textContent = `Failed to load diff: ${err}`;
  }
}

//#endregion

//#region Side-by-side diff rendering

function renderFilePair(detailEl, pair) {
  detailEl.textContent = "";
  if (pair.original === pair.current) {
    // Contents are identical: the change is mode/permission-only.
    detailEl.textContent =
      "No content changes — this file was changed by permissions or metadata only.";
    return;
  }
  if (pair.original.includes("\u0000") || pair.current.includes("\u0000")) {
    detailEl.textContent = "Binary file — no text diff available.";
    return;
  }
  detailEl.appendChild(renderSideBySide(pair.original, pair.current));
}

function splitLines(text) {
  const lines = text.split("\n");
  if (lines[lines.length - 1] === "") {
    lines.pop();
  }
  return lines;
}

function alignLines(a, b) {
  const al = splitLines(a);
  const bl = splitLines(b);
  const n = al.length;
  const m = bl.length;

  let dp = null;
  if (n * m <= 4000000) {
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

function renderSideBySide(original, current) {
  const rows = alignLines(original, current);

  const table = document.createElement("table");
  table.className = "side-by-side";

  // Contiguous runs of changed rows are grouped into their own tbody so the
  // ^/v buttons can scroll block by block.
  let tbody = document.createElement("tbody");
  let inBlock = false;
  for (const row of rows) {
    const changed =
      (row.left && row.left.type !== "same") ||
      (row.right && row.right.type !== "same");
    if (changed !== inBlock) {
      table.appendChild(tbody);
      tbody = document.createElement("tbody");
      if (changed) {
        tbody.className = "diff-block";
      }
      inBlock = changed;
    }
    const tr = document.createElement("tr");
    for (const side of ["left", "right"]) {
      const cell = document.createElement("td");
      const line = row[side];
      if (line) {
        cell.className = `line-${line.type}`;
        const num = document.createElement("span");
        num.className = "line-num";
        num.textContent = line.num;
        const text = document.createElement("span");
        text.className = "line-text";
        text.textContent = line.text;
        cell.appendChild(num);
        cell.appendChild(text);
      }
      tr.appendChild(cell);
    }
    tbody.appendChild(tr);
  }
  table.appendChild(tbody);
  return table;
}

//#endregion

//#region Action wiring (buttons + delegated listeners)

const recloneBtn = document.getElementById("reclone-btn");
recloneBtn.onclick = () => {
  if (confirm("Reclone this repo? The directory will be DELETED and cloned fresh from origin.\n\nAll local branches, stashes, tags, and unpushed commits will be lost.")) {
    sendAction("Reclone");
  }
};

document.getElementById("pull-btn").onclick = () => sendAction("Pull");
document.getElementById("push-btn").onclick = () => sendAction("Push");
document.getElementById("fetch-btn").onclick = () => sendAction("Fetch");

document.getElementById("remove-tab-btn").onclick = () => {
  const tab = lastState && activeTab(lastState);
  if (!tab) return;
  if (confirm(`Remove "${tab.name}"? The repository on disk is kept.`)) {
    // Send the explicit id: removal must not depend on selection state.
    sendRaw(JSON.stringify({ tab: tab.id, action: "CloseTab" }));
    activeTabId = null;
    awaitingNewTab = false;
    showAddForm = false;
  }
};

const commitMsg = document.getElementById("commit-msg");
document.getElementById("stage-commit-push-btn").onclick = () => {
  if (!commitMsg.value.trim()) return;
  sendAction({ CommitAllPush: commitMsg.value.trim() });
  commitMsg.value = "";
};
document.getElementById("commit-btn").onclick = () => {
  if (!commitMsg.value.trim()) return;
  sendAction({ CommitAll: commitMsg.value.trim() });
  commitMsg.value = "";
};
document.getElementById("commit-push-btn").onclick = () => {
  if (!commitMsg.value.trim()) return;
  sendAction({ CommitAllPush: commitMsg.value.trim() });
  commitMsg.value = "";
};
document.getElementById("discard-all-btn").onclick = () => {
  if (confirm("Discard all uncommitted changes?")) {
    sendAction("DiscardAll");
  }
};

document.getElementById("tabs").addEventListener("click", (event) => {
  const btn = event.target.closest(".tab");
  if (!btn || !lastState) return;
  if (btn.classList.contains("new-tab")) {
    // The "+" toggles the local Add Repository view; no server round-trip.
    showAddForm = true;
    updateUrlTab(null);
    if (lastState) render(lastState);
    return;
  }
  activeTabId = Number(btn.dataset.tabId);
  showAddForm = false;
  updateUrlTab(activeTabId);
  render(lastState);
});

document.getElementById("changes").addEventListener("click", (event) => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (!tab) return;
  const actionBtn = event.target.closest(".action-btn");
  if (actionBtn) {
    const path = actionBtn.dataset.path;
    if (actionBtn.dataset.action === "prev-block") {
      stepDiffBlock(actionBtn.closest(".change-head"), -1);
    } else if (actionBtn.dataset.action === "next-block") {
      stepDiffBlock(actionBtn.closest(".change-head"), 1);
    } else if (actionBtn.dataset.action === "discard") {
      if (confirm(`Discard changes to ${path}?`)) {
        if (actionBtn.dataset.status === "Untracked") {
          sendAction({ DiscardUntracked: path });
        } else {
          sendAction({ Discard: path });
        }
      }
    } else if (actionBtn.dataset.staged === "true") {
      sendAction({ Unstage: path });
    } else {
      sendAction({ Stage: path });
    }
    return;
  }
  const head = event.target.closest(".change-head");
  if (!head) return;
  toggleDiff(head.nextElementSibling, tab, head.dataset.path);
});

document.getElementById("history").addEventListener("click", (event) => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (!tab) return;
  const head = event.target.closest(".commit-head");
  if (!head) return;
  toggleCommitActions(head.nextElementSibling, tab, head.dataset.hash);
});

document.getElementById("history-search").addEventListener("input", (event) => {
  historyQuery = event.target.value;
  if (historySearchTimer !== null) clearTimeout(historySearchTimer);
  const query = historyQuery.trim();
  historySearchTimer = setTimeout(() => {
    historySearchTimer = null;
    if (lastState && activeTabId !== null) {
      // An empty query restores the default history window; a non-empty
      // one asks the daemon to run `git log --grep` over full history.
      sendRaw(JSON.stringify({ tab: activeTabId, action: { SearchHistory: query } }));
    }
  }, query ? 300 : 150);
});

document.querySelectorAll(".section-title").forEach((title) => {
  title.onclick = () => {
    const section = title.parentElement;
    section.classList.toggle("collapsed");
    const arrow = title.querySelector(".arrow");
    arrow.innerHTML = section.classList.contains("collapsed") ? "&#9652;" : "&#9662;";
  };
});

document.getElementById("branch-list").addEventListener("click", (event) => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (!tab) return;
  const actionBtn = event.target.closest(".branch-action-btn");
  if (actionBtn) {
    const branch = actionBtn.dataset.branch;
    if (actionBtn.dataset.action === "delete") {
      if (confirm(`Delete branch "${branch}"?`)) {
        sendAction({ DeleteBranch: branch });
      }
    } else if (actionBtn.dataset.action === "checkout") {
      sendAction({ CheckoutBranch: branch });
    }
  }
});

document.getElementById("create-branch-btn").onclick = () => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (!tab) return;
  const name = document.getElementById("branch-filter").value.trim();
  if (!name) return;
  const hash = tab.state.history && tab.state.history.length > 0
    ? tab.state.history[0].hash
    : null;
  if (hash) {
    sendAction({ CreateBranch: [name, hash] });
  } else {
    sendAction({ CreateBranch: [name, "HEAD"] });
  }
  document.getElementById("branch-filter").value = "";
  renderBranches(tab);
};

document.getElementById("branch-filter").addEventListener("input", () => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (tab) renderBranches(tab);
});
//#endregion

const ws = new WebSocket(`ws://${location.host}/ws`);

let activeTabId = null;
let lastState = null;
let expandedKey = null;
let expandedDetailEl = null;
let expandedCommitKey = null;
let expandedCommitEl = null;
let awaitingNewTab = false;
let browserDir = null;
let browserParent = null;
const knownTabIds = new Set();
const commitCache = new Map();
const pairCache = new Map();

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
    document.getElementById("folder-browser").style.display = "none";
  }

  const browseBtn = document.getElementById("browse-folder-btn");
  const browserEl = document.getElementById("folder-browser");
  const browserCurrent = document.getElementById("browser-current");
  const browserEntries = document.getElementById("browser-entries");
  const upBtn = document.getElementById("browser-up-btn");
  const homeBtn = document.getElementById("browser-home-btn");
  const selectBtn = document.getElementById("browser-select-btn");
  const openBtn = document.getElementById("open-repo-btn");
  const cancelBtn = document.getElementById("cancel-repo-btn");

  nameInput.oninput = () => { nameInput.dataset.userSet = "true"; };

  async function loadBrowser(path) {
    try {
      const url = path ? `/browse?path=${encodeURIComponent(path)}` : "/browse";
      const response = await fetch(url);
      const data = await response.json();
      browserDir = data.current;
      browserParent = data.parent;
      browserCurrent.textContent = data.current;
      upBtn.disabled = !data.parent;
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

  browseBtn.onclick = () => {
    const show = browserEl.style.display === "none";
    browserEl.style.display = show ? "block" : "none";
    if (show && !browserDir) {
      loadBrowser(pathInput.value.trim());
    }
  };
  upBtn.onclick = () => { if (browserParent) loadBrowser(browserParent); };
  homeBtn.onclick = () => loadBrowser("");
  selectBtn.onclick = () => {
    if (!browserDir) return;
    pathInput.value = browserDir;
    if (!nameInput.dataset.userSet) {
      nameInput.value = getTabNameFromPath(browserDir);
    }
    browserEl.style.display = "none";
  };

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
    sendAction({ NewTab: JSON.stringify({ name: nameInput.value.trim(), path }) });
  };

  cancelBtn.onclick = () => sendAction("CloseTab");
}

ws.onmessage = (event) => {
  const state = JSON.parse(event.data);
  if (activeTabId === null) {
    activeTabId = state.active;
  }
  const newIds = state.tabs.map((t) => t.id).filter((id) => !knownTabIds.has(id));
  if (awaitingNewTab && newIds.length > 0) {
    activeTabId = newIds[0];
    awaitingNewTab = false;
  }
  for (const id of state.tabs.map((t) => t.id)) {
    knownTabIds.add(id);
  }
  const prev = lastState;
  lastState = state;
  if (prev !== null && JSON.stringify(prev) === JSON.stringify(state)) {
    return;
  }
  render(state);
};

function sendAction(action) {
  ws.send(JSON.stringify({ tab: activeTabId, action }));
}

function activeTab(state) {
  return state.tabs.find((t) => t.id === activeTabId) ?? state.tabs[0];
}

function render(state) {
  renderTabBar(state);

  if (state.tabs.length === 0) {
    return;
  }
  const tab = activeTab(state);
  
  const addRepoForm = document.getElementById("add-repo-form");
  const repoView = document.getElementById("repo-view");
  const historySection = document.getElementById("history-section");
  const actionsSection = document.getElementById("actions");
  
  if (tab && tab.repo_path === "") {
    addRepoForm.style.display = "block";
    repoView.style.display = "none";
    historySection.style.display = "none";
    actionsSection.style.display = "none";
    setupAddRepoForm(tab);
    document.title = "Grit | New Repository";
    return;
  }
  addRepoForm.style.display = "none";
  repoView.style.display = "block";
  historySection.style.display = "block";
  actionsSection.style.display = "block";
  document.title = `Grit | ${tab.name}`;
  document.getElementById("overview").textContent =
    `${tab.state.current_branch} — ${tab.state.changes.length} change(s)`;

  const changesEl = document.getElementById("changes");
  let stillOpen = false;
  expandedDetailEl = null;
  if (tab.state.changes.length === 0) {
    changesEl.textContent = "Working tree clean";
  } else {
    changesEl.textContent = "";
    for (const change of tab.state.changes) {
      if (appendChangeRow(changesEl, change, tab)) stillOpen = true;
    }
  }
  if (!stillOpen) {
    expandedKey = null;
    expandedDetailEl = null;
  }

  const historyEl = document.getElementById("history");
  historyEl.textContent = "";
  let commitStillOpen = false;
  expandedCommitEl = null;
  for (const commit of tab.state.history) {
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

function renderTabBar(state) {
  const tabsEl = document.getElementById("tabs");
  tabsEl.textContent = "";
  for (const tab of state.tabs) {
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

async function toggleDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  if (expandedKey === key) {
    expandedKey = null;
    expandedDetailEl.style.display = "none";
    expandedDetailEl.textContent = "";
    expandedDetailEl = null;
    return;
  }
  if (expandedDetailEl) {
    expandedDetailEl.style.display = "none";
    expandedDetailEl.textContent = "";
  }
  expandedKey = key;
  expandedDetailEl = detailEl;
  detailEl.style.display = "block";
  await showDiff(detailEl, tab, path);
}

async function showDiff(detailEl, tab, path) {
  const key = `${tab.id}:${path}`;
  const cached = pairCache.get(key);
  if (cached) {
    detailEl.textContent = "";
    detailEl.appendChild(renderSideBySide(cached.original, cached.current));
    return;
  }
  detailEl.textContent = "Loading...";
  try {
    const response = await fetch(`/files?tab=${tab.id}&path=${encodeURIComponent(path)}`);
    const pair = await response.json();
    pairCache.set(key, pair);
    detailEl.textContent = "";
    detailEl.appendChild(renderSideBySide(pair.original, pair.current));
  } catch (err) {
    detailEl.textContent = `Failed to load diff: ${err}`;
  }
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

  const thead = document.createElement("thead");
  const headRow = document.createElement("tr");
  for (const label of ["Original", "New"]) {
    const th = document.createElement("th");
    th.textContent = label;
    headRow.appendChild(th);
  }
  thead.appendChild(headRow);
  table.appendChild(thead);

  const tbody = document.createElement("tbody");
  for (const row of rows) {
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

const nukeBtn = document.getElementById("nuke-btn");
nukeBtn.onclick = () => {
  if (confirm("Nuke this repo? All local changes will be discarded and the repo reset to origin.")) {
    sendAction("Nuke");
  }
};

document.getElementById("pull-btn").onclick = () => sendAction("Pull");
document.getElementById("push-btn").onclick = () => sendAction("Push");
document.getElementById("fetch-btn").onclick = () => sendAction("Fetch");

const commitMsg = document.getElementById("commit-msg");
  document.getElementById("stage-commit-push-btn").onclick = () => {
    if (!commitMsg.value.trim()) return;
    sendAction({ CommitAllPush: commitMsg.value.trim() });
    commitMsg.value = "";
  };
  document.getElementById("commit-push-btn").onclick = () => {
    if (!commitMsg.value.trim()) return;
    const msg = commitMsg.value.trim();
    sendAction({ Commit: msg });
    sendAction("Push");
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
    awaitingNewTab = true;
    sendAction({ NewTab: JSON.stringify({ name: "new", path: "" }) });
    return;
  }
  activeTabId = Number(btn.dataset.tabId);
  render(lastState);
});

document.getElementById("changes").addEventListener("click", (event) => {
  if (!lastState) return;
  const tab = activeTab(lastState);
  if (!tab) return;
  const actionBtn = event.target.closest(".action-btn");
  if (actionBtn) {
    const path = actionBtn.dataset.path;
    if (actionBtn.dataset.action === "discard") {
      if (confirm(`Discard changes to ${path}?`)) {
        sendAction({ Discard: path });
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

document.querySelectorAll(".section-title").forEach((title) => {
  title.onclick = () => {
    const section = title.parentElement;
    section.classList.toggle("collapsed");
    const arrow = title.querySelector(".arrow");
    arrow.innerHTML = section.classList.contains("collapsed") ? "&#9652;" : "&#9662;";
  };
});
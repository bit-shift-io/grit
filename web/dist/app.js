const ws = new WebSocket(`ws://${location.host}/ws`);

let nukeArmed = false;
let activeTabId = null;
let lastState = null;
let expandedKey = null;
let expandedDetailEl = null;
let expandedCommitKey = null;
let expandedCommitEl = null;
const commitCache = new Map();
const pairCache = new Map();

ws.onmessage = (event) => {
  const state = JSON.parse(event.data);
  if (activeTabId === null) {
    activeTabId = state.active;
  }
  lastState = state;
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

  const tab = activeTab(state);
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
      const key = `${tab.id}:${change.path}`;
      const row = document.createElement("div");
      row.className = "change-row";

      const head = document.createElement("div");
      head.className = "change-head";

      const status = document.createElement("span");
      status.className = "change-status";
      status.textContent = change.status;

      const file = document.createElement("span");
      file.className = "change-file";
      file.textContent = change.path;

      const action = document.createElement("button");
      action.className = "action-btn";
      if (change.is_staged) {
        action.textContent = "Unstage";
        action.onclick = () => sendAction({ Unstage: change.path });
      } else {
        action.textContent = "Stage";
        action.onclick = () => sendAction({ Stage: change.path });
      }

      head.appendChild(status);
      head.appendChild(file);
      head.appendChild(action);

      const detail = document.createElement("div");
      detail.className = "change-diff";
      const expanded = expandedKey === key;
      if (expanded) {
        stillOpen = true;
        expandedDetailEl = detail;
      }
      detail.style.display = expanded ? "block" : "none";
      if (expanded) {
        showDiff(detail, tab, change.path);
      }

      row.appendChild(head);
      row.appendChild(detail);
      changesEl.appendChild(row);

      head.onclick = (event) => {
        if (event.target.closest(".action-btn")) return;
        toggleDiff(detail, tab, change.path);
      };
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

    head.onclick = () => toggleCommitActions(actions, tab, commit.hash);
  }
  if (!commitStillOpen) {
    expandedCommitKey = null;
    expandedCommitEl = null;
  }
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
  showCommitSummary(actionsEl, tab, hash);
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
    btn.onclick = () => {
      activeTabId = tab.id;
      nukeArmed = false;
      nukeStatus.textContent = "";
      nukeBtn.textContent = "Nuke";
      render(state);
    };
    tabsEl.appendChild(btn);
  }
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
const nukeStatus = document.getElementById("nuke-status");
nukeBtn.onclick = () => {
  if (!nukeArmed) {
    nukeArmed = true;
    nukeStatus.textContent = "Are you sure? Click Nuke again to wipe the repo.";
    nukeBtn.textContent = "Confirm Nuke";
    return;
  }
  nukeArmed = false;
  nukeStatus.textContent = "";
  nukeBtn.textContent = "Nuke";
  sendAction("Nuke");
};

document.getElementById("pull-btn").onclick = () => sendAction("Pull");
document.getElementById("push-btn").onclick = () => sendAction("Push");
document.getElementById("fetch-btn").onclick = () => sendAction("Fetch");

const commitMsg = document.getElementById("commit-msg");
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

document.querySelectorAll(".section-title").forEach((title) => {
  title.onclick = () => {
    const section = title.parentElement;
    section.classList.toggle("collapsed");
    const arrow = title.querySelector(".arrow");
    arrow.innerHTML = section.classList.contains("collapsed") ? "&#9652;" : "&#9662;";
  };
});
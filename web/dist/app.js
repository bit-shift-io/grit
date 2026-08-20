const ws = new WebSocket(`ws://${location.host}/ws`);

let nukeArmed = false;
let activeTabId = null;
let lastState = null;
let expandedKey = null;
let expandedDetailEl = null;
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
  for (const commit of tab.state.history) {
    const item = document.createElement("div");
    item.className = "history-item";
    item.textContent = `${commit.hash.slice(0, 8)} ${commit.author} - ${commit.message}`;
    historyEl.appendChild(item);
  }
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

document.querySelectorAll(".section-title").forEach((title) => {
  title.onclick = () => {
    const section = title.parentElement;
    section.classList.toggle("collapsed");
    const arrow = title.querySelector(".arrow");
    arrow.innerHTML = section.classList.contains("collapsed") ? "&#9652;" : "&#9662;";
  };
});
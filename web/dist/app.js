import {
  HISTORY_SEARCH_FILLED_MS,
  HISTORY_SEARCH_EMPTY_MS,
  BRANCH_FILTER_DEBOUNCE_MS,
  store,
  activeTab,
  sendAction,
  sendRaw,
  render,
  renderTabBar,
  updateUrlTab,
  renderBranches,
  renderStashes,
  toggleSection,
  openSocket,
  registerCoreHandlers,
  commitMsg,
} from "./common.js";
import { toggleDiff, showDiff, stepDiffBlock } from "./diff.js";
import { showView, setView, initTerminal } from "./terminal.js";
import { initSettings } from "./settings.js";

//#region Action wiring (buttons + delegated listeners)

// ======================================
// Button Wiring & Event Listeners
// ======================================
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
  const tab = store.lastState && activeTab(store.lastState);
  if (!tab) return;
  if (confirm(`Remove "${tab.name}"? The repository on disk is kept.`)) {
    // Send the explicit id: removal must not depend on selection state.
    sendRaw(JSON.stringify({ tab: tab.id, action: "CloseTab" }));
    store.activeTabId = null;
    store.awaitingNewTab = false;
    store.showAddForm = false;
  }
};

// Sends a commit-family action, holding the message in `pendingCommit` instead
// of clearing the box outright. Whether the message was consumed is git's call,
// not ours: a failed `add -A` or `commit` means it was never used and has to
// survive for a retry, while a committed message must be cleared so the next one
// starts from empty.
function sendCommitAction(build) {
  const message = commitMsg.value.trim();
  if (!message) return;
  const tab = store.lastState ? activeTab(store.lastState) : null;
  store.pendingCommit = {
    message,
    tabId: tab ? tab.id : null,
    afterSeq: tab ? Math.max(0, ...(tab.log || []).map((e) => e.seq)) : 0,
  };
  sendAction(build(message));
}

document.getElementById("stage-commit-push-btn").onclick = () =>
  sendCommitAction((m) => ({ CommitAllPush: m }));
document.getElementById("commit-btn").onclick = () => sendCommitAction((m) => ({ Commit: m }));
document.getElementById("commit-push-btn").onclick = () => sendCommitAction((m) => ({ CommitPush: m }));
document.getElementById("discard-all-btn").onclick = () => {
  if (confirm("Discard all uncommitted changes?")) {
    sendAction("DiscardAll");
  }
};

document.getElementById("tabs").addEventListener("click", (event) => {
  const btn = event.target.closest("[data-tab-id], .new-tab, .settings-btn");
  if (!btn) return;

  // The gear is global: it works with or without a selected tab.
  if (btn.classList.contains("settings-btn")) {
    setView("settings");
    return;
  }
  if (!store.lastState) return;

  // Handle "+" new tab button
  if (btn.classList.contains("new-tab")) {
    store.showAddForm = true;
    updateUrlTab(null);
    render(store.lastState);
    return;
  }

  const tabId = Number(btn.dataset.tabId);
  const view = btn.dataset.view;

  if (!isNaN(tabId)) {
    store.activeTabId = tabId;
    updateUrlTab(store.activeTabId);
    store.showAddForm = false;
    if (btn.classList.contains("tab-name")) {
      setView("dashboard");
    } else if (btn.classList.contains("tab-view-btn")) {
      setView(view);
    }
    render(store.lastState);
  }
});

document.getElementById("changes").addEventListener("click", (event) => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
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
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
  if (!tab) return;
  const head = event.target.closest(".commit-head");
  if (!head) return;
  toggleCommitActions(head.nextElementSibling, tab, head.dataset.hash);
});

document.getElementById("history-search").addEventListener("input", (event) => {
  store.historyQuery = event.target.value;
  if (store.historySearchTimer !== null) clearTimeout(store.historySearchTimer);
  const query = store.historyQuery.trim();
  store.historySearchTimer = setTimeout(() => {
    store.historySearchTimer = null;
    if (store.lastState && store.activeTabId !== null) {
      // An empty query restores the default history window; a non-empty
      // one asks the daemon to run `git log --grep` over full history.
      sendRaw(JSON.stringify({ tab: store.activeTabId, action: { SearchHistory: query } }));
    }
  }, query ? HISTORY_SEARCH_FILLED_MS : HISTORY_SEARCH_EMPTY_MS);
});

document.querySelectorAll(".section-title").forEach((title) => {
  title.onclick = () => {
    const section = title.parentElement;
    const isExpanded = !section.classList.contains("collapsed");
    section.classList.toggle("collapsed");
    // Update ARIA attributes
    title.setAttribute("aria-expanded", String(!isExpanded));
    // Arrow is now controlled via CSS ::before pseudo-element
  };
});

document.getElementById("branch-list").addEventListener("click", (event) => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
  if (!tab) return;
  const actionBtn = event.target.closest(".branch-action-btn");
  if (actionBtn) {
    const branch = actionBtn.dataset.branch;
    if (actionBtn.dataset.action === "delete") {
      const row = actionBtn.closest(".branch-row");
      const isRemote = row && row.classList.contains("remote");
      if (isRemote) {
        const name = branch.split("/").slice(1).join("/");
        if (confirm(`Delete remote branch "${branch}"? This removes it from origin.`)) {
          sendAction({ DeleteBranch: { name, local: false, remote: true } });
        }
      } else {
        const name = branch;
        if (confirm(`Delete branch "${branch}" on remote also?`)) {
          sendAction({ DeleteBranch: { name, local: true, remote: true } });
        } else {
          sendAction({ DeleteBranch: { name, local: true, remote: false } });
        }
      }
    } else if (actionBtn.dataset.action === "checkout") {
      sendAction({ CheckoutBranch: branch });
    } else if (actionBtn.dataset.action === "merge") {
      if (confirm(`Merge branch "${branch}" into "${tab.state.current_branch}" and push?`)) {
        sendAction({ Merge: branch });
      }
    }
  }
});

document.getElementById("create-branch-btn").onclick = () => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
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
  if (store.branchFilterTimer !== null) clearTimeout(store.branchFilterTimer);
  store.branchFilterTimer = setTimeout(() => {
    store.branchFilterTimer = null;
    if (!store.lastState) return;
    const tab = activeTab(store.lastState);
    if (tab) renderBranches(tab);
  }, BRANCH_FILTER_DEBOUNCE_MS);
});

document.getElementById("stash-list").addEventListener("click", (event) => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
  if (!tab) return;
  const head = event.target.closest(".stash-head");
  if (!head) return;
  const key = `${tab.id}:${head.dataset.id}`;
  toggleSection("stash", key, head.nextElementSibling, () => renderStashes(tab));
});

document.getElementById("create-stash-btn").onclick = () => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
  if (!tab) return;
  const msg = document.getElementById("stash-input").value.trim();
  sendAction({ StashPush: msg });
  document.getElementById("stash-input").value = "";
};

document.getElementById("run-script-btn").onclick = () => {
  const relPath = document.getElementById("script-select").value;
  if (!relPath) return;
  sendAction({ RunScript: relPath });
};

document.getElementById("clear-log-btn").onclick = () => {
  if (!store.lastState) return;
  const tab = activeTab(store.lastState);
  const maxSeq = Math.max(0, ...(tab.log || []).map((e) => e.seq));
  store.clearedUpToSeq = maxSeq;
  render(store.lastState);
};
//#endregion

// ======================================
// Boot
// ======================================
function main() {
  registerCoreHandlers({ showView, showDiff });
  openSocket();
  initTerminal();
  initSettings();
  showView("dashboard");
  renderTabBar({ tabs: [] });
}

main();

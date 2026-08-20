const ws = new WebSocket(`ws://${location.host}/ws`);
ws.onmessage = (event) => {
  const state = JSON.parse(event.data);
  document.getElementById("branch").textContent = state.current_branch;
  document.getElementById("changes").textContent =
    `${state.changes.length} change(s)`;
};

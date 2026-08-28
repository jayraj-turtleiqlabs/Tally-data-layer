import { invoke } from "@tauri-apps/api/core";

interface AgentStatus {
  paired: boolean;
  company_name: string | null;
  tally_reachable: boolean;
  sync_idle: boolean;
  sync_in_progress: boolean;
  last_successful_sync: string | null;
  last_error: string | null;
  last_known_alter_id: number;
  backfill_complete: boolean;
}

const pairSection = document.getElementById("pair-section")!;
const pairedSection = document.getElementById("paired-section")!;
const connectionBadge = document.getElementById("connection-badge")!;
const pairingCodeInput = document.getElementById("pairing-code") as HTMLInputElement;
const pairBtn = document.getElementById("pair-btn") as HTMLButtonElement;
const pairError = document.getElementById("pair-error")!;
const companyName = document.getElementById("company-name")!;
const tallyStatus = document.getElementById("tally-status")!;
const lastSync = document.getElementById("last-sync")!;
const alterId = document.getElementById("alter-id")!;
const syncError = document.getElementById("sync-error")!;
const syncBtn = document.getElementById("sync-btn") as HTMLButtonElement;
const disconnectBtn = document.getElementById("disconnect-btn") as HTMLButtonElement;

function formatDate(iso: string | null): string {
  if (!iso) return "Never";
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return "Never";
  }
}

function setBadge(status: AgentStatus) {
  connectionBadge.className = "badge";
  if (status.sync_in_progress) {
    connectionBadge.textContent = "Syncing…";
    connectionBadge.classList.add("badge-syncing");
  } else if (status.last_error) {
    connectionBadge.textContent = "Error";
    connectionBadge.classList.add("badge-error");
  } else if (status.paired) {
    connectionBadge.textContent = "Paired";
    connectionBadge.classList.add("badge-paired");
  } else {
    connectionBadge.textContent = "Not paired";
    connectionBadge.classList.add("badge-unpaired");
  }
}

function renderStatus(status: AgentStatus) {
  setBadge(status);

  if (status.paired) {
    pairSection.classList.add("hidden");
    pairedSection.classList.remove("hidden");
    companyName.textContent = status.company_name ?? "Connected company";
    tallyStatus.textContent = status.tally_reachable ? "Reachable" : "Unreachable";
    lastSync.textContent = formatDate(status.last_successful_sync);
    alterId.textContent = String(status.last_known_alter_id);

    if (status.last_error) {
      syncError.textContent = status.last_error;
      syncError.classList.remove("hidden");
    } else {
      syncError.classList.add("hidden");
    }

    syncBtn.disabled = status.sync_in_progress;
    syncBtn.textContent = status.sync_in_progress ? "Syncing…" : "Sync Now";
  } else {
    pairSection.classList.remove("hidden");
    pairedSection.classList.add("hidden");
  }
}

async function refreshStatus() {
  try {
    const status = await invoke<AgentStatus>("get_agent_status");
    renderStatus(status);
  } catch {
    connectionBadge.textContent = "Error";
    connectionBadge.className = "badge badge-error";
  }
}

pairBtn.addEventListener("click", async () => {
  const code = pairingCodeInput.value.trim();
  if (code.length !== 8) {
    pairError.textContent = "Pairing failed. Please check your code and try again.";
    pairError.classList.remove("hidden");
    return;
  }

  pairBtn.disabled = true;
  pairError.classList.add("hidden");

  try {
    const company = await invoke<string>("pair_agent", { code });
    companyName.textContent = company;
    pairingCodeInput.value = "";
    await refreshStatus();
  } catch {
    pairError.textContent = "Pairing failed. Please check your code and try again.";
    pairError.classList.remove("hidden");
  } finally {
    pairBtn.disabled = false;
  }
});

syncBtn.addEventListener("click", async () => {
  syncBtn.disabled = true;
  syncError.classList.add("hidden");

  try {
    await invoke("sync_now");
  } catch (e) {
    syncError.textContent =
      typeof e === "string" ? e : "Sync failed. Please try again.";
    syncError.classList.remove("hidden");
  } finally {
    await refreshStatus();
  }
});

disconnectBtn.addEventListener("click", async () => {
  if (!confirm("Disconnect this agent and clear pairing?")) return;
  await invoke("disconnect_agent");
  await refreshStatus();
});

refreshStatus();
setInterval(refreshStatus, 5000);

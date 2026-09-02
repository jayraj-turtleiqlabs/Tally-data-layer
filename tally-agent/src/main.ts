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

interface DeviceAuthPublicSession {
  user_code: string;
  verification_uri: string;
  browser_opened: boolean;
}

const pairSection = document.getElementById("pair-section")!;
const pairDefaultView = document.getElementById("pair-default-view")!;
const browserAuthView = document.getElementById("browser-auth-view")!;
const pairedSection = document.getElementById("paired-section")!;
const connectionBadge = document.getElementById("connection-badge")!;

const browserLoginBtn = document.getElementById("browser-login-btn") as HTMLButtonElement;
const userCodeDisplay = document.getElementById("user-code-display")!;
const browserAuthStatus = document.getElementById("browser-auth-status")!;
const browserAuthLink = document.getElementById("browser-auth-link") as HTMLAnchorElement;
const browserAuthError = document.getElementById("browser-auth-error")!;
const cancelBrowserBtn = document.getElementById("cancel-browser-btn") as HTMLButtonElement;
const pairError = document.getElementById("pair-error")!;

const companyName = document.getElementById("company-name")!;
const tallyStatus = document.getElementById("tally-status")!;
const lastSync = document.getElementById("last-sync")!;
const alterId = document.getElementById("alter-id")!;
const syncError = document.getElementById("sync-error")!;
const syncBtn = document.getElementById("sync-btn") as HTMLButtonElement;
const disconnectBtn = document.getElementById("disconnect-btn") as HTMLButtonElement;

let isPollingDeviceAuth = false;

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
  } else if (status.last_error && status.paired) {
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

    if (status.last_error) {
      pairError.textContent = status.last_error;
      pairError.classList.remove("hidden");
    }
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

function resetBrowserAuthView() {
  isPollingDeviceAuth = false;
  browserAuthView.classList.add("hidden");
  pairDefaultView.classList.remove("hidden");
  browserAuthError.classList.add("hidden");
  browserLoginBtn.disabled = false;
}

browserLoginBtn.addEventListener("click", async () => {
  browserLoginBtn.disabled = true;
  pairError.classList.add("hidden");
  browserAuthError.classList.add("hidden");

  try {
    const session = await invoke<DeviceAuthPublicSession>("initiate_device_login");
    
    // Switch to browser auth view
    pairDefaultView.classList.add("hidden");
    browserAuthView.classList.remove("hidden");
    
    userCodeDisplay.textContent = session.user_code;
    browserAuthLink.href = session.verification_uri;
    browserAuthStatus.textContent = "Waiting for you to approve in your browser…";
    isPollingDeviceAuth = true;

    // Start polling backend for approval
    try {
      const company = await invoke<string>("poll_device_login");
      if (isPollingDeviceAuth) {
        browserAuthStatus.textContent = "Approved! Setting up your connection…";
        companyName.textContent = company;
        await refreshStatus();
        resetBrowserAuthView();
      }
    } catch (pollErr) {
      if (isPollingDeviceAuth) {
        const errorMsg = typeof pollErr === "string" ? pollErr : "Login was denied or expired — try again";
        browserAuthError.textContent = errorMsg;
        browserAuthError.classList.remove("hidden");
        browserAuthStatus.textContent = "Authorization failed";
      }
    }
  } catch (initErr) {
    const errorMsg = typeof initErr === "string" ? initErr : "Failed to start browser login. Ensure Tally is running.";
    pairError.textContent = errorMsg;
    pairError.classList.remove("hidden");
    browserLoginBtn.disabled = false;
  }
});

cancelBrowserBtn.addEventListener("click", async () => {
  isPollingDeviceAuth = false;
  try {
    await invoke("cancel_device_login");
  } catch {
    // Ignore cancel errors
  }
  resetBrowserAuthView();
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

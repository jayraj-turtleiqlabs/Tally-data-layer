import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

interface CompanyItem {
  company_name: string;
  company_guid?: string | null;
  alter_id: number;
  is_active_in_tally: boolean;
  is_connected: boolean;
  connection_id?: string | null;
  status: string;
  last_successful_sync?: string | null;
  last_known_alter_id: number;
  last_error?: string | null;
  sync_in_progress: boolean;
  backfill_complete: boolean;
}

interface DeviceAuthPublicSession {
  user_code: string;
  verification_uri: string;
  browser_opened: boolean;
}

interface AppSettings {
  autostart_enabled: boolean;
  auto_sync_enabled: boolean;
  auto_sync_interval_mins: number;
  snooze_until?: string | null;
}

interface SyncReminderState {
  show_reminder: boolean;
  reason?: string | null;
  company_name?: string | null;
  connection_id?: string | null;
  last_attempt_at?: string | null;
  last_successful_sync?: string | null;
}

// Header & Main Elements
const tallyStatusBadge = document.getElementById("tally-status-badge")!;
const refreshBtn = document.getElementById("refresh-btn") as HTMLButtonElement;
const settingsToggleBtn = document.getElementById("settings-toggle-btn") as HTMLButtonElement;
const discoveringIndicator = document.getElementById("discovering-indicator")!;
const companiesList = document.getElementById("companies-list")!;
const toast = document.getElementById("toast")!;
const toastMessage = document.getElementById("toast-message")!;

// Settings Elements
const settingsPanel = document.getElementById("settings-panel")!;
const closeSettingsBtn = document.getElementById("close-settings-btn") as HTMLButtonElement;
const autostartToggle = document.getElementById("autostart-toggle") as HTMLInputElement;
const autoSyncIntervalSelect = document.getElementById("auto-sync-interval-select") as HTMLSelectElement;

// Tally Offline Modal Elements
const tallyOfflineModal = document.getElementById("tally-offline-modal")!;
const checkTallyAgainBtn = document.getElementById("check-tally-again-btn") as HTMLButtonElement;
const dismissTallyModalBtn = document.getElementById("dismiss-tally-modal-btn") as HTMLButtonElement;

// Sync Reminder Modal Elements
const syncReminderModal = document.getElementById("sync-reminder-modal")!;
const syncReminderReason = document.getElementById("sync-reminder-reason")!;
const reminderCompanyName = document.getElementById("reminder-company-name")!;
const reminderLastSync = document.getElementById("reminder-last-sync")!;
const reminderSyncNowBtn = document.getElementById("reminder-sync-now-btn") as HTMLButtonElement;
const reminderSnoozeBtn = document.getElementById("reminder-snooze-btn") as HTMLButtonElement;

// Auth Modal Elements
const authModal = document.getElementById("auth-modal")!;
const authModalTitle = document.getElementById("auth-modal-title")!;
const userCodeDisplay = document.getElementById("user-code-display")!;
const browserAuthStatus = document.getElementById("browser-auth-status")!;
const browserAuthLink = document.getElementById("browser-auth-link") as HTMLAnchorElement;
const browserAuthError = document.getElementById("browser-auth-error")!;
const cancelBrowserBtn = document.getElementById("cancel-browser-btn") as HTMLButtonElement;

let isPollingAuth = false;
let toastTimeout: number | null = null;
let lastTallyReachableState: boolean | null = null;
let lastToastShownTime = 0;
let currentSettings: AppSettings = {
  autostart_enabled: true,
  auto_sync_enabled: true,
  auto_sync_interval_mins: 60,
};
let activeReminderConnectionId: string | null = null;

function showToast(message: string, durationMs = 3500) {
  const now = Date.now();
  if (now - lastToastShownTime < 6000 && toastMessage.textContent === message && !toast.classList.contains("hidden")) {
    return;
  }
  lastToastShownTime = now;
  toastMessage.textContent = message;
  toast.classList.remove("hidden");
  if (toastTimeout) {
    clearTimeout(toastTimeout);
  }
  toastTimeout = window.setTimeout(() => {
    toast.classList.add("hidden");
  }, durationMs);
}

function formatDate(iso?: string | null): string {
  if (!iso) return "Never";
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return "Never";
  }
}

// -------------------------------------------------------------
// Settings Management
// -------------------------------------------------------------

async function loadSettings() {
  try {
    currentSettings = await invoke<AppSettings>("get_app_settings");
    autostartToggle.checked = currentSettings.autostart_enabled;
    autoSyncIntervalSelect.value = currentSettings.auto_sync_enabled
      ? String(currentSettings.auto_sync_interval_mins)
      : "0";
  } catch (err) {
    console.error("Failed to load settings:", err);
  }
}

async function persistSettings() {
  try {
    currentSettings.autostart_enabled = autostartToggle.checked;
    const intervalVal = parseInt(autoSyncIntervalSelect.value, 10);
    currentSettings.auto_sync_interval_mins = isNaN(intervalVal) ? 60 : intervalVal;
    currentSettings.auto_sync_enabled = currentSettings.auto_sync_interval_mins > 0;

    await invoke("update_app_settings", { settings: currentSettings });
    showToast("Preferences saved");
  } catch (err) {
    console.error("Failed to save settings:", err);
    showToast("Failed to save preferences");
  }
}

settingsToggleBtn.addEventListener("click", () => {
  settingsPanel.classList.toggle("hidden");
});

closeSettingsBtn.addEventListener("click", () => {
  settingsPanel.classList.add("hidden");
});

autostartToggle.addEventListener("change", () => {
  persistSettings();
});

autoSyncIntervalSelect.addEventListener("change", () => {
  persistSettings();
});

// -------------------------------------------------------------
// Tally Offline Modal
// -------------------------------------------------------------

function openTallyOfflineModal() {
  tallyOfflineModal.classList.remove("hidden");
}

function closeTallyOfflineModal() {
  tallyOfflineModal.classList.add("hidden");
}

dismissTallyModalBtn.addEventListener("click", () => {
  closeTallyOfflineModal();
});

checkTallyAgainBtn.addEventListener("click", async () => {
  checkTallyAgainBtn.disabled = true;
  checkTallyAgainBtn.textContent = "Checking…";
  try {
    const isReachable = await invoke<boolean>("check_tally_status");
    if (isReachable) {
      closeTallyOfflineModal();
      showToast("Tally is reachable!");
      await refreshCompaniesReal();
    } else {
      showToast("Tally is still not running on port 9000");
    }
  } catch (e) {
    showToast("Connection test failed");
  } finally {
    checkTallyAgainBtn.disabled = false;
    checkTallyAgainBtn.textContent = "Check Again";
  }
});

// -------------------------------------------------------------
// Data Sync Reminder Modal
// -------------------------------------------------------------

async function checkSyncReminder() {
  try {
    const reminder = await invoke<SyncReminderState>("get_sync_reminder");
    if (reminder.show_reminder) {
      activeReminderConnectionId = reminder.connection_id || null;
      syncReminderReason.textContent = reminder.reason || "Auto-sync could not complete.";
      reminderCompanyName.textContent = reminder.company_name || "Connected Company";
      reminderLastSync.textContent = formatDate(reminder.last_successful_sync);
      syncReminderModal.classList.remove("hidden");
    } else {
      syncReminderModal.classList.add("hidden");
    }
  } catch (err) {
    console.error("Failed to check sync reminder:", err);
  }
}

reminderSnoozeBtn.addEventListener("click", async () => {
  try {
    await invoke("dismiss_sync_reminder", { snoozeMinutes: 15 });
    syncReminderModal.classList.add("hidden");
    showToast("Reminder snoozed for 15 minutes");
  } catch (e) {
    console.error("Failed to snooze reminder:", e);
  }
});

reminderSyncNowBtn.addEventListener("click", async () => {
  // Verify Tally is reachable first
  const isReachable = await invoke<boolean>("check_tally_status");
  if (!isReachable) {
    openTallyOfflineModal();
    return;
  }

  reminderSyncNowBtn.disabled = true;
  reminderSyncNowBtn.textContent = "Syncing…";

  try {
    await invoke("dismiss_sync_reminder", { snoozeMinutes: null });
    syncReminderModal.classList.add("hidden");

    if (activeReminderConnectionId) {
      await invoke("sync_company", { connectionId: activeReminderConnectionId });
    } else {
      await invoke("sync_now");
    }
    showToast("Sync completed successfully!");
  } catch (err) {
    console.error("Reminder sync failed:", err);
    showToast(String(err));
  } finally {
    reminderSyncNowBtn.disabled = false;
    reminderSyncNowBtn.textContent = "Sync Now";
    await loadCompaniesCached();
  }
});

// -------------------------------------------------------------
// UI Rendering & Company List
// -------------------------------------------------------------

function updateTallyBadge(companies: CompanyItem[]) {
  const isReachable = companies.some((c) => c.status !== "Offline" && !c.status.includes("Offline"));
  tallyStatusBadge.className = "badge";
  if (isReachable) {
    tallyStatusBadge.textContent = "● Tally Reachable";
    tallyStatusBadge.classList.add("badge-paired");
    if (lastTallyReachableState === false) {
      toast.classList.add("hidden");
    }
    lastTallyReachableState = true;
  } else {
    tallyStatusBadge.textContent = "○ Tally Offline";
    tallyStatusBadge.classList.add("badge-error");
    lastTallyReachableState = false;
  }
}

function renderCompanies(companies: CompanyItem[]) {
  updateTallyBadge(companies);
  companiesList.innerHTML = "";

  if (companies.length === 0) {
    companiesList.innerHTML = `
      <div class="empty-card">
        <p class="empty-title">No Companies Found</p>
        <p class="empty-hint">Open a company in Tally to connect.</p>
      </div>
    `;
    return;
  }

  for (const company of companies) {
    const card = document.createElement("div");
    card.className = "card company-card";

    if (company.is_connected) {
      const isOnlineActive = company.is_active_in_tally && company.status.includes("Active");
      const isOffline = company.status === "Offline" || company.status.includes("Offline");
      const isUnavailable = company.status.includes("Not currently available") || company.status.includes("Not Open");
      const isSyncing = company.sync_in_progress;

      let badgeClass = "badge-paired";
      let statusLabel = "● Connected · Active";

      if (isOffline) {
        badgeClass = "badge-error";
        statusLabel = "○ Offline";
      } else if (isUnavailable) {
        badgeClass = "badge-unpaired";
        statusLabel = "○ Connected · Not currently available";
      } else if (isSyncing) {
        badgeClass = "badge-syncing";
        statusLabel = "⏳ Syncing…";
      } else if (!company.is_active_in_tally) {
        badgeClass = "badge-unpaired";
        statusLabel = "○ Connected · Inactive";
      }

      let hintText = "";
      if (isUnavailable) {
        hintText = `<p class="inactive-hint">Tally does not have this company loaded. Open ${escapeHtml(company.company_name)} in Tally to sync.</p>`;
      } else if (!company.is_active_in_tally && !isOffline) {
        hintText = `<p class="inactive-hint">Tally currently has another company open. Open ${escapeHtml(company.company_name)} in Tally to sync.</p>`;
      }

      card.innerHTML = `
        <div class="company-card-header">
          <div>
            <h3 class="company-title">${escapeHtml(company.company_name)}</h3>
            <span class="company-meta">${company.company_guid ? `GUID: ${escapeHtml(company.company_guid)}` : "Tally"}</span>
          </div>
          <span class="badge ${badgeClass}">${statusLabel}</span>
        </div>

        ${hintText}

        <dl class="status-grid">
          <dt>Last sync</dt>
          <dd>${formatDate(company.last_successful_sync)}</dd>
          <dt>Checkpoint</dt>
          <dd>${company.last_known_alter_id}</dd>
        </dl>

        ${company.last_error ? `<div class="error-banner">${escapeHtml(company.last_error)}</div>` : ""}

        <div class="card-actions">
          <button class="btn primary sync-company-btn" data-cid="${company.connection_id}" ${!isOnlineActive || isSyncing ? "disabled" : ""} title="${!isOnlineActive ? `Open ${escapeHtml(company.company_name)} in Tally to sync` : ""}">
            ${isSyncing ? "Syncing…" : "Sync Now"}
          </button>
          <button class="btn secondary full-resync-company-btn" data-cid="${company.connection_id}" data-name="${escapeHtml(company.company_name)}" ${!isOnlineActive || isSyncing ? "disabled" : ""} title="Reset checkpoint and re-sync all records from beginning">
            Full Resync
          </button>
          <button class="btn secondary disconnect-company-btn" data-cid="${company.connection_id}" data-name="${escapeHtml(company.company_name)}">
            Disconnect
          </button>
        </div>
      `;
    } else {
      const isOnlineActive = company.is_active_in_tally;
      const statusLabel = isOnlineActive ? "● Found in Tally · Active" : "○ Found in Tally · Inactive";
      const badgeClass = isOnlineActive ? "badge-paired" : "badge-idle";

      card.innerHTML = `
        <div class="company-card-header">
          <div>
            <h3 class="company-title">${escapeHtml(company.company_name)}</h3>
            <span class="company-meta">${company.company_guid ? `GUID: ${escapeHtml(company.company_guid)}` : "Tally"}</span>
          </div>
          <span class="badge ${badgeClass}">${statusLabel}</span>
        </div>

        <p class="available-hint">${isOnlineActive ? "Currently active in Tally" : "Open in Tally background"}</p>

        <div class="card-actions">
          <button class="btn primary connect-company-btn" data-name="${escapeHtml(company.company_name)}" data-guid="${escapeHtml(company.company_guid || "")}">
            Connect
          </button>
        </div>
      `;
    }

    companiesList.appendChild(card);
  }

  attachCardEvents();
}

function escapeHtml(text: string): string {
  const div = document.createElement("div");
  div.textContent = text;
  return div.innerHTML;
}

function attachCardEvents() {
  // Sync buttons
  document.querySelectorAll(".sync-company-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      const target = e.currentTarget as HTMLButtonElement;
      const cid = target.getAttribute("data-cid");
      if (!cid) return;

      // Verify Tally is reachable first
      const isReachable = await invoke<boolean>("check_tally_status");
      if (!isReachable) {
        openTallyOfflineModal();
        return;
      }

      target.disabled = true;
      target.textContent = "Syncing…";

      try {
        await invoke("sync_company", { connectionId: cid });
        showToast("Sync completed!");
      } catch (err) {
        console.error("Sync failed:", err);
        showToast(String(err));
      } finally {
        await loadCompaniesCached();
      }
    });
  });

  // Full resync buttons
  document.querySelectorAll(".full-resync-company-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      const target = e.currentTarget as HTMLButtonElement;
      const cid = target.getAttribute("data-cid");
      const name = target.getAttribute("data-name") || "this company";
      if (!cid) return;

      const isReachable = await invoke<boolean>("check_tally_status");
      if (!isReachable) {
        openTallyOfflineModal();
        return;
      }

      if (!confirm(`Force a full re-sync for ${name}? This will reset the checkpoint to 0 and re-push all records.`)) return;

      target.disabled = true;
      target.textContent = "Resyncing…";

      try {
        await invoke("full_resync_company", { connectionId: cid });
        showToast("Full resync completed!");
      } catch (err) {
        console.error("Full resync failed:", err);
        showToast(String(err));
      } finally {
        await loadCompaniesCached();
      }
    });
  });

  // Disconnect buttons
  document.querySelectorAll(".disconnect-company-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      const target = e.currentTarget as HTMLButtonElement;
      const cid = target.getAttribute("data-cid");
      const name = target.getAttribute("data-name") || "this company";
      if (!cid) return;

      if (!confirm(`Disconnect ${name}? Checkpoints will be preserved.`)) return;

      try {
        await invoke("disconnect_company", { connectionId: cid });
      } catch (err) {
        console.error("Disconnect failed:", err);
      } finally {
        await loadCompaniesCached();
      }
    });
  });

  // Connect buttons
  document.querySelectorAll(".connect-company-btn").forEach((btn) => {
    btn.addEventListener("click", async (e) => {
      const target = e.currentTarget as HTMLButtonElement;
      const name = target.getAttribute("data-name");
      const guid = target.getAttribute("data-guid") || null;
      if (!name) return;

      const isReachable = await invoke<boolean>("check_tally_status");
      if (!isReachable) {
        openTallyOfflineModal();
        return;
      }

      await startConnectCompany(name, guid);
    });
  });
}

async function startConnectCompany(companyName: string, companyGuid?: string | null) {
  authModalTitle.textContent = `Connect ${companyName}`;
  browserAuthError.classList.add("hidden");
  browserAuthStatus.textContent = "Waiting for you to approve in your browser…";
  authModal.classList.remove("hidden");

  try {
    const session = await invoke<DeviceAuthPublicSession>("initiate_company_connection", {
      companyName,
      companyGuid: companyGuid || null,
    });

    userCodeDisplay.textContent = session.user_code;
    browserAuthLink.href = session.verification_uri;
    isPollingAuth = true;

    try {
      await invoke<string>("poll_company_connection", {
        companyName,
        companyGuid: companyGuid || null,
      });

      if (isPollingAuth) {
        browserAuthStatus.textContent = "Approved! Initializing connection…";
        setTimeout(() => {
          closeAuthModal();
          refreshCompaniesReal();
        }, 800);
      }
    } catch (pollErr) {
      if (isPollingAuth) {
        const errorMsg = typeof pollErr === "string" ? pollErr : "Login was denied or expired — try again";
        browserAuthError.textContent = errorMsg;
        browserAuthError.classList.remove("hidden");
        browserAuthStatus.textContent = "Authorization failed";
      }
    }
  } catch (initErr) {
    const errorMsg = typeof initErr === "string" ? initErr : "Failed to initiate connection. Ensure Tally is running.";
    browserAuthError.textContent = errorMsg;
    browserAuthError.classList.remove("hidden");
    browserAuthStatus.textContent = "Initialization failed";
  }
}

function closeAuthModal() {
  isPollingAuth = false;
  authModal.classList.add("hidden");
  invoke("cancel_device_login").catch(() => {});
}

cancelBrowserBtn.addEventListener("click", () => {
  closeAuthModal();
});

async function loadCompaniesCached() {
  try {
    const companies = await invoke<CompanyItem[]>("get_companies");
    renderCompanies(companies);
  } catch (e) {
    console.error("Failed to load cached companies:", e);
  }
}

async function refreshCompaniesReal() {
  discoveringIndicator.classList.remove("hidden");
  refreshBtn.disabled = true;

  try {
    const companies = await invoke<CompanyItem[]>("refresh_companies");
    renderCompanies(companies);
  } catch (e) {
    console.error("Failed to refresh companies:", e);
    showToast("Tally is not reachable");
  } finally {
    discoveringIndicator.classList.add("hidden");
    refreshBtn.disabled = false;
  }
}

refreshBtn.addEventListener("click", () => {
  refreshCompaniesReal();
});

// -------------------------------------------------------------
// Tauri Events & App Initialization
// -------------------------------------------------------------

// Listen for Tally offline reminder from backend
listen("tally-offline-detected", () => {
  openTallyOfflineModal();
});

// Listen for background auto-sync reminder changes
listen("sync-reminder-changed", () => {
  checkSyncReminder();
});

// Initialize settings and companies on launch
loadSettings();
refreshCompaniesReal();
checkSyncReminder();

// Fast local UI polling from in-memory cache without hammering Tally
setInterval(loadCompaniesCached, 5000);

// Background refresh of Tally discovery every 45s
setInterval(refreshCompaniesReal, 45000);

// Check sync reminder status every 30s
setInterval(checkSyncReminder, 30000);

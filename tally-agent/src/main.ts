import { invoke } from "@tauri-apps/api/core";

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

const tallyStatusBadge = document.getElementById("tally-status-badge")!;
const refreshBtn = document.getElementById("refresh-btn") as HTMLButtonElement;
const discoveringIndicator = document.getElementById("discovering-indicator")!;
const companiesList = document.getElementById("companies-list")!;
const toast = document.getElementById("toast")!;
const toastMessage = document.getElementById("toast-message")!;

// Auth modal elements
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

function showToast(message: string, durationMs = 3500) {
  const now = Date.now();
  // Prevent spamming within 8 seconds with the same message
  if (now - lastToastShownTime < 8000 && toastMessage.textContent === message && !toast.classList.contains("hidden")) {
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
    if (lastTallyReachableState !== false) {
      showToast("Tally is not running");
    }
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
      // Connected Card
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
          <button class="btn secondary disconnect-company-btn" data-cid="${company.connection_id}" data-name="${escapeHtml(company.company_name)}">
            Disconnect
          </button>
        </div>
      `;
    } else {
      // Discovered / Unconnected Card
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

      target.disabled = true;
      target.textContent = "Syncing…";

      try {
        await invoke("sync_company", { connectionId: cid });
      } catch (err) {
        console.error("Sync failed:", err);
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

// Initialize on app startup
refreshCompaniesReal();

// Fast local UI polling from in-memory cache without hammering Tally
setInterval(loadCompaniesCached, 5000);

// Background refresh of Tally discovery every 30s
setInterval(refreshCompaniesReal, 30000);


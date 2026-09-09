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
const emptyState = document.getElementById("empty-state")!;
const emptyRefreshBtn = document.getElementById("empty-refresh-btn") as HTMLButtonElement;

// Auth modal elements
const authModal = document.getElementById("auth-modal")!;
const authModalTitle = document.getElementById("auth-modal-title")!;
const userCodeDisplay = document.getElementById("user-code-display")!;
const browserAuthStatus = document.getElementById("browser-auth-status")!;
const browserAuthLink = document.getElementById("browser-auth-link") as HTMLAnchorElement;
const browserAuthError = document.getElementById("browser-auth-error")!;
const cancelBrowserBtn = document.getElementById("cancel-browser-btn") as HTMLButtonElement;

let isPollingAuth = false;

function formatDate(iso?: string | null): string {
  if (!iso) return "Never";
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return "Never";
  }
}

function updateTallyBadge(companies: CompanyItem[]) {
  const isReachable = companies.some((c) => c.status !== "Offline");
  tallyStatusBadge.className = "badge";
  if (isReachable) {
    tallyStatusBadge.textContent = "● Tally Reachable";
    tallyStatusBadge.classList.add("badge-paired");
  } else {
    tallyStatusBadge.textContent = "○ Tally Offline";
    tallyStatusBadge.classList.add("badge-error");
  }
}

function renderCompanies(companies: CompanyItem[]) {
  updateTallyBadge(companies);
  companiesList.innerHTML = "";

  if (companies.length === 0) {
    companiesList.classList.add("hidden");
    emptyState.classList.remove("hidden");
    return;
  }

  emptyState.classList.add("hidden");
  companiesList.classList.remove("hidden");

  for (const company of companies) {
    const card = document.createElement("div");
    card.className = "card company-card";

    if (company.is_connected) {
      // Connected Card
      const isOnlineActive = company.is_active_in_tally && company.status.includes("Active");
      const isOffline = company.status === "Offline" || company.status.includes("Offline");
      const isSyncing = company.sync_in_progress;

      let badgeClass = "badge-paired";
      let statusLabel = "● Connected · Active";

      if (isOffline) {
        badgeClass = "badge-error";
        statusLabel = "○ Offline";
      } else if (isSyncing) {
        badgeClass = "badge-syncing";
        statusLabel = "⏳ Syncing…";
      } else if (!company.is_active_in_tally) {
        badgeClass = "badge-unpaired";
        statusLabel = "○ Connected · Inactive";
      }

      card.innerHTML = `
        <div class="company-card-header">
          <div>
            <h3 class="company-title">${escapeHtml(company.company_name)}</h3>
            <span class="company-meta">${company.company_guid ? `GUID: ${escapeHtml(company.company_guid)}` : "Tally"}</span>
          </div>
          <span class="badge ${badgeClass}">${statusLabel}</span>
        </div>

        ${!company.is_active_in_tally && !isOffline ? `<p class="inactive-hint">Tally currently has another company open. Open ${escapeHtml(company.company_name)} in Tally to sync.</p>` : ""}

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
      const statusLabel = isOnlineActive ? "● Found in Tally · Not connected" : "○ Found in Tally · Not connected";
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
  } finally {
    discoveringIndicator.classList.add("hidden");
    refreshBtn.disabled = false;
  }
}

refreshBtn.addEventListener("click", () => {
  refreshCompaniesReal();
});

emptyRefreshBtn.addEventListener("click", () => {
  refreshCompaniesReal();
});

// Initialize on app startup
refreshCompaniesReal();

// Fast local UI polling from in-memory cache without hammering Tally
setInterval(loadCompaniesCached, 5000);


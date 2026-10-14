# End-to-End Smoke Test Checklist: Tally Agent & Web Dashboard

This checklist guides manual verification of the Tally Agent (`tally-agent`) and Web Dashboard (`fininsight-web`) against a local Unified Accounting Gateway running at `http://localhost:3000`.

---

## Prerequisites

1. **Unified Accounting Gateway**: Running at `http://localhost:3000`.
2. **Tally ERP 9 or TallyPrime**:
   - Running with ODBC/XML server enabled on port 9000 (`127.0.0.1:9000`).
   - At least one test company loaded (e.g. `Acme Corp Ltd`).
3. **Web Dashboard**:
   - Running at `http://localhost:5173` (via `cd fininsight-web && npm run dev`).
   - Logged in as an `OWNER` or `ADMIN` user.

---

## Smoke Test Steps

### Step 1: Environment Setup and Agent Startup

1. Open PowerShell and set the API base URL to your local gateway:
   ```powershell
   $env:FININSIGHT_API_BASE = "http://localhost:3000"
   $env:TALLY_PORT = "9000"
   cd tally-agent
   npm run tauri dev
   ```
2. Verify Tally is accessible by opening Tally on port 9000 with your company open.

- **Expected UI State**: The Tally Agent system tray icon appears and the agent status window shows the active company as `Available · Active` or `Available · Inactive`.
- **Expected Agent Log Line**:
  ```text
  [INFO] FinInsight Tally Agent v0.1.0 started (Tally port: 9000)
  [INFO] Discovered active company: 'Acme Corp Ltd'
  ```

---

### Step 2: Device Flow Initiation & Approval

1. In the Agent UI, click **Connect Company** or **Sign In / Connect**.
2. The agent initiates RFC 8628 device flow with `POST /api/v1/agent/device/initiate` and displays a `user_code` (e.g. `WDJB-4K9P`) and verification URL `http://localhost:3000/device`.
3. Open a browser to `http://localhost:3000/device?user_code=WDJB-4K9P` (or enter code manually).
4. Approve the device connection as an authenticated `OWNER` or `ADMIN`.

- **Expected UI State**:
  - Web: Approval confirmation screen appears.
  - Agent: Transitions from "Waiting for approval..." to "Connected · Active".
- **Expected Agent Log Lines**:
  ```text
  [INFO] [device_auth] Session initiated (user_code: WDJB-4K9P, poll_interval: 6s, expires_in: 600s)
  [INFO] [device_auth] Starting device authorization polling loop (interval: 6s)
  [INFO] [device_auth] Device authorization Approved on iteration #... (has_token=true, connection_id='conn-...')
  [INFO] [pair] Storing fresh agent token in vault for connection_id='conn-...'
  ```

---

### Step 3: Vault Storage & Initial Backfill Sync

1. Confirm credentials are saved in Windows Credential Manager under the connection ID.
2. In the Agent tray menu or status window, click **Sync Now**.
3. The agent verifies the active company against the profile, pulls vouchers and ledgers via Export-only envelopes, and pushes batches to `POST /api/v1/agent/sync/delta`.

- **Expected UI State**: Agent status window shows sync progress, then returns to "Idle", with "Last Sync" displaying current timestamp.
- **Expected Agent Log Lines**:
  ```text
  [INFO] [sync] Starting initial backfill for company 'Acme Corp Ltd' (connection_id='conn-...')
  [INFO] [sync] POST /api/v1/agent/sync/delta (initial batch 1/1) -> HTTP 200
  [INFO] [backfill] Initial backfill complete: ... records pushed. Checkpoint advanced to alter_id=...
  ```
- **Expected Gateway Response**: HTTP 200 `{"success": true, "alterId": ..., "recordsProcessed": ...}`.

---

### Step 4: Dashboard Triggered Sync ("Sync Now" via Heartbeat)

1. Navigate to the Web Dashboard at `http://localhost:5173`.
2. Locate the active connection in the Connections list / Pairing Panel.
3. Click the **Sync Now** button.
4. Verify the web client sends `POST /api/v1/connections/:id/sync` (returns 200 `{ message, connection_id, request_id }`).
5. Wait for the agent's periodic heartbeat (~45s interval) or inspect the heartbeat response.

- **Expected UI State**:
  - Web: Button displays "Sync requested" notification.
  - Agent: Automatically triggers delta sync without manual user intervention.
- **Expected Agent Log Lines**:
  ```text
  [DEBUG] [heartbeat] Acknowledged for connection_id='conn-...' (company='Acme Corp Ltd', is_active=true)
  [INFO] [heartbeat] Remote sync requested by dashboard for connection_id='conn-...'. Triggering delta sync...
  [INFO] Delta sync from ALTERID > ...
  [INFO] Delta sync complete: ... records pushed. Checkpoint advanced.
  ```

---

### Step 5: Failed Batch & Checkpoint Safety Check (Negative Test)

1. Configure or stub the gateway to return HTTP 422 (or HTTP 200 with `{"success": false, "error": "Invalid batch"}`) on `POST /api/v1/agent/sync/delta`.
2. In the Agent, trigger a sync by clicking **Sync Now**.
3. Inspect the agent's checkpoint file in `%APPDATA%\fininsight-tally-agent\checkpoints\checkpoint_<connection_id>.json` (or dev path).

- **Expected UI State**: Agent displays an error banner ("Sync failed").
- **Expected Agent Log Lines**:
  ```text
  [WARN] [cloud_client] Sync push response indicated failure (or non-2xx status code)
  [ERROR] Sync failed: SyncFailed(...)
  ```
- **Expected Checkpoint State**: The `last_known_alter_id` in the checkpoint file remains unchanged from before the failed sync.

---

### Step 6: Revocation and Disconnect

1. In the Agent status window, click **Disconnect** for the active connection.
2. The agent calls `POST /api/v1/agent/revoke` with the connection's Bearer token and `{ "connection_id": "conn-..." }`.
3. Check the gateway connection status and the agent local state.

- **Expected UI State**:
  - Agent: Connection removed; status updates to "Not paired" / "Available".
  - Web Dashboard: Connection shows as `DISCONNECTED` or revoked.
- **Expected Agent Log Lines**:
  ```text
  [INFO] Disconnecting connection_id='conn-...'
  [DEBUG] POST /api/v1/agent/revoke -> HTTP 200
  [INFO] Cleared credentials and checkpoint profile for connection_id='conn-...'
  ```

# FinInsight Tally Agent

Windows desktop system-tray agent that bridges a locally installed **Tally ERP 9 / TallyPrime** instance to the FinInsight cloud backend. The agent runs in the background, pulls financial data according to user preferences or on demand, and never exposes Tally data outside the paired FinInsight organization.

## Security Guarantees

| Requirement | Implementation |
|---|---|
| Tally endpoint is loopback only | `TallyEndpoint::new()` rejects non-`127.0.0.1` hosts at startup |
| No inbound connections | All cloud communication is outbound HTTPS; no listening sockets |
| Read-only against Tally | `EnvelopeDirection` enum has **only** `Export` — no Import variant exists |
| One-way cloud pipe | `CloudClient` only POSTs (device auth, sync push, heartbeat) — no data fetch |
| Token in OS vault | `keyring` crate → Windows Credential Manager |
| Fail loud | Typed errors; no cached/mock/synthesized data on Tally failure |
| Checkpoint on ack only | `CheckpointStore::advance_on_ack()` called only after HTTP 200 |
| Build-time API URL | `FININSIGHT_API_BASE` env var at compile time |
| Redacted logs | `redact.rs` strips tokens, XML bodies, balances, names |
| Code signing | `tauri.conf.json` & `scripts/build-exe.js` → Authenticode thumbprint |

## Prerequisites

- Windows 10/11
- [Rust](https://rustup.rs/) (1.77+)
- [Node.js](https://nodejs.org/) 18+
- Tally ERP 9 or TallyPrime with ODBC/XML server enabled on port 9000
- FinInsight backend running (dev or production)

## Setup

```powershell
cd tally-agent
npm install
```

### Environment Variables (compile-time)

```powershell
# Development
$env:FININSIGHT_API_BASE = "http://localhost:3000"

# Production (example)
$env:FININSIGHT_API_BASE = "https://api.fininsight.example.com"
```

Optional runtime:

```powershell
$env:TALLY_PORT = "9000"   # default: 9000
```

### Code Signing (release builds)

Set before building:

```powershell
$env:TAURI_SIGNING_CERTIFICATE_THUMBPRINT = "YOUR_CERT_THUMBPRINT"
$env:CERT_THUMBPRINT = "YOUR_CERT_THUMBPRINT"
```

> **Do not distribute unsigned binaries.** Use `cargo tauri build` or `npm run build:exe` only with a valid signing certificate for release.

## Development

```powershell
# Step 1: Test Tally client in isolation
cd src-tauri
cargo test tally_client tally_schema tally_envelope -- --nocapture

# Step 2: Test vault roundtrip
cargo test vault -- --nocapture

# Full agent with UI
cd ..
npm run tauri dev
```

## Features & Implementation

1. `tally_client.rs` + `tally_envelope.rs` + `tally_schema.rs` — Export-only Tally XML
2. `vault.rs` — OS credential vault
3. `cloud_client.rs` — device auth, sync push, heartbeat
4. Initial backfill (automatic post-pairing, batched)
5. On-demand sync (tray "Sync Now" button)
6. Heartbeat loop (45s interval, decoupled from sync)
7. Tray UI + status window
8. Redacted logging
9. Signing + updater config scaffolding
10. Auto-start on boot via Windows Registry (`HKCU\Software\Microsoft\Windows\CurrentVersion\Run`)
11. App settings persistence (`settings.json`) & background auto-sync engine
12. Tally offline modal reminders on startup & sync
13. Auto-sync failure & missed sync interactive pop-up reminders ("Sync Now" / "Remind Me Later")

## Project Structure

```
tally-agent/
├── src/                    # Tray UI (Vite + TypeScript)
│   ├── index.html          # UI with preferences & interactive reminder modals
│   ├── main.ts             # Event listeners & state management
│   └── styles.css          # Theme, preferences dropdown, & modal dialog styles
├── src-tauri/
│   ├── src/
│   │   ├── main.rs         # Entry, tray, Tauri commands, startup reachability
│   │   ├── lib.rs          # Agent state, heartbeat & sync reminder states
│   │   ├── settings.rs     # App settings persistence (autostart, interval, snooze)
│   │   ├── autostart.rs    # Windows Registry auto-start manager
│   │   ├── auto_sync.rs    # Periodic background auto-sync runner & reminder alerts
│   │   ├── tally_client.rs # Local Tally XML (Export-only)
│   │   ├── tally_envelope.rs # Type-level Export-only envelopes
│   │   ├── tally_schema.rs # ERP9 vs TallyPrime adapter
│   │   ├── cloud_client.rs # Device auth, sync push, heartbeat
│   │   ├── vault.rs        # Windows Credential Manager
│   │   ├── checkpoint.rs   # ALTERID checkpoint persistence
│   │   ├── sync.rs         # Backfill + on-demand sync orchestration
│   │   ├── redact.rs       # Log redaction
│   │   ├── logging.rs      # Rotating redacted log file
│   │   └── errors.rs       # Typed error enums
│   ├── tests/fixtures/     # XML fixtures for unit tests
│   └── tauri.conf.json     # Signing + updater scaffolding
├── scripts/
│   └── build-exe.js        # Packaging & Authenticode code signing script
└── README.md
```

## API Endpoints Used

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/v1/agent/device/initiate` | Initiate device-flow pairing |
| POST | `/api/v1/agent/device/poll` | Poll device pairing status |
| POST | `/api/v1/agent/sync/delta` | Backfill batches & incremental delta sync |
| POST | `/api/v1/agent/heartbeat` | Liveness, sliding token extension & sync_requested poll |
| POST | `/api/v1/agent/revoke` | Revoke agent connection |

## Sync & Automation Model

- **Background Auto-Sync**: The agent includes an integrated auto-sync background worker configured per user preferences.
- **Interval Options**: Configurable (every 15m, 30m, 1h, 2h, 4h, 8h, or disabled) directly from the UI Preferences dropdown.
- **Tally Offline Pop-Up**: If Tally is detected as offline upon agent startup (or when initiating sync), an interactive modal reminds the user to open Tally first with a 1-click "Check Again" button.
- **Sync Failure & Omission Reminders**: If a scheduled auto-sync cannot complete (e.g. Tally closed or network error), the agent surfaces a persistent reminder modal offering "Sync Now" or "Remind Me Later" (snooze).
- **Heartbeat vs Sync**: Heartbeat continues to report reachability every ~45s without pulling financial data.

## Code Signing & SmartScreen Protection

To prevent Windows SmartScreen "Unknown Publisher" / "Windows protected your PC" warnings when distributing the agent, refer to the detailed guide in [`docs/temp/temp.md`](../docs/temp/temp.md), covering:
1. Microsoft Store submission flow (automatic trust, no code signing cert required).
2. Direct Web Distribution using Azure Trusted Signing or EV Code Signing certificates.
3. Automated Authenticode signing in `scripts/build-exe.js`.

## TODO

- [ ] Auto-update implementation (config scaffold exists in `tauri.conf.json`)
- [ ] Tauri icon assets (`src-tauri/icons/`)
- [ ] End-to-end test against live FinInsight backend

## Running Tests

```powershell
cd src-tauri
cargo test
```

Tests cover: XML fixture parsing, Export-only envelope enforcement, checkpoint advance/rollback, redaction, vault roundtrip, and Tally unreachable error handling.

# FinInsight Tally Agent

Windows desktop system-tray agent that bridges a locally installed **Tally ERP 9 / TallyPrime** instance to the FinInsight cloud backend. The agent runs in the background, pulls financial data **only when explicitly triggered**, and never exposes Tally data outside the paired FinInsight organization.

## Security Guarantees

| Requirement | Implementation |
|---|---|
| Tally endpoint is loopback only | `TallyEndpoint::new()` rejects non-`127.0.0.1` hosts at startup |
| No inbound connections | All cloud communication is outbound HTTPS; no listening sockets |
| Read-only against Tally | `EnvelopeDirection` enum has **only** `Export` — no Import variant exists |
| One-way cloud pipe | `CloudClient` only POSTs (pair, sync push, heartbeat) — no data fetch |
| Token in OS vault | `keyring` crate → Windows Credential Manager |
| Fail loud | Typed errors; no cached/mock/synthesized data on Tally failure |
| Checkpoint on ack only | `CheckpointStore::advance_on_ack()` called only after HTTP 200 |
| Build-time API URL | `FININSIGHT_API_BASE` env var at compile time |
| Redacted logs | `redact.rs` strips tokens, XML bodies, balances, names |
| Code signing | `tauri.conf.json` → `certificateThumbprint` placeholder |

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
```

> **Do not distribute unsigned binaries.** Use `cargo tauri build` only with a valid signing certificate for release.

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

## Build Order (as implemented)

1. `tally_client.rs` + `tally_envelope.rs` + `tally_schema.rs` — Export-only Tally XML
2. `vault.rs` — OS credential vault
3. `cloud_client.rs` — pairing, sync push, heartbeat
4. Initial backfill (automatic post-pairing, batched)
5. On-demand sync (tray "Sync Now" button — **no background sync timer**)
6. Heartbeat loop (45s interval, decoupled from sync)
7. Tray UI + status window
8. Redacted logging
9. Signing + updater config scaffolding

## Project Structure

```
tally-agent/
├── src/                    # Tray/pairing UI (Vite + TypeScript)
├── src-tauri/
│   ├── src/
│   │   ├── main.rs         # Entry, tray, Tauri commands
│   │   ├── lib.rs          # Agent state, heartbeat loop
│   │   ├── tally_client.rs # Local Tally XML (Export-only)
│   │   ├── tally_envelope.rs # Type-level Export-only envelopes
│   │   ├── tally_schema.rs # ERP9 vs TallyPrime adapter
│   │   ├── cloud_client.rs # Pairing, sync push, heartbeat
│   │   ├── vault.rs        # Windows Credential Manager
│   │   ├── checkpoint.rs   # ALTERID checkpoint persistence
│   │   ├── sync.rs         # Backfill + on-demand sync orchestration
│   │   ├── redact.rs       # Log redaction
│   │   ├── logging.rs      # Rotating redacted log file
│   │   └── errors.rs       # Typed error enums
│   ├── tests/fixtures/     # XML fixtures for unit tests
│   └── tauri.conf.json     # Signing + updater scaffolding
└── README.md
```

## API Endpoints Used

| Method | Path | Purpose |
|---|---|---|
| POST | `/api/v1/agent/pair` | Pair with 8-char code |
| POST | `/api/v1/sync/initial` | Initial backfill batches |
| POST | `/api/v1/sync/delta` | Incremental sync push |
| POST | `/api/v1/agent/heartbeat` | Liveness + optional sync-request poll |

## Sync Model

- **No automatic background sync.** The agent stays idle until the user clicks **Sync Now** (tray menu or status window).
- Dashboard-triggered sync is supported via the `sync_requested` flag returned on heartbeat (agent polls every ~45s).
- Heartbeat reports Tally reachability only — it does not pull or push financial data.

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

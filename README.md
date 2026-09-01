# FinInsight Tally Connector

Monorepo containing two components of the FinInsight ↔ Tally integration:

| Directory | Description |
|---|---|
| [`tally-agent/`](tally-agent/) | Windows desktop system-tray agent (Tauri 2 + Rust) — bridges local Tally to FinInsight cloud |
| [`fininsight-web/`](fininsight-web/) | React web dashboard — login, pairing, team management, connection switching |

## Architecture

```
┌─────────────┐     Export XML      ┌──────────────┐     HTTPS POST      ┌─────────────────┐
│ Tally ERP9/ │ ◄────────────────── │ FinInsight   │ ──────────────────► │ FinInsight      │
│ TallyPrime  │   127.0.0.1:9000    │ Tally Agent  │  (pair/sync/beat)   │ Backend API     │
└─────────────┘                     └──────────────┘                     └─────────────────┘
                                           ▲                                      ▲
                                           │ User: Sync Now                       │ JWT auth
                                           │ (no auto-sync timer)                 │
                                    ┌──────┴───────┐                    ┌─────────┴────────┐
                                    │ System Tray  │                    │ Web Dashboard    │
                                    │ + Status UI  │                    │ (fininsight-web) │
                                    └──────────────┘                    └──────────────────┘
```

## Key Design Decisions

1. **On-demand sync only** — no background sync timer; user clicks "Sync Now" or dashboard sets a poll flag
2. **Export-only Tally access** — enforced at the type level (`EnvelopeDirection::Export` only)
3. **One-way cloud pipe** — agent only pushes data; never reads from backend
4. **Separate credentials** — human JWT vs agent token, never mixed
5. **Loopback-only Tally** — agent refuses non-`127.0.0.1` endpoints

## Quick Start

### Prerequisites

- Windows 10/11
- [Rust](https://rustup.rs/) 1.77+
- Node.js 18+
- Tally with XML server on port 9000
- FinInsight backend running

### Tally Agent

```powershell
cd tally-agent
npm install
$env:FININSIGHT_API_BASE = "http://localhost:3000"
npm run tauri dev
```

### Web Dashboard

```powershell
cd fininsight-web
npm install
npm run dev
```

## Build Order

See [`tally-agent/README.md`]
(tally-agent/README.md) for the agent implementation sequence and test plan.

### To build the exe file

$env:FININSIGHT_API_BASE="base-url-link 'for eg: https://fininsight-api-vzv5.onrender.com' "
npm run build:exe

## Security Checklist

- [x] Tally endpoint validated as loopback at startup
- [x] No Import XML envelopes (type-level enforcement)
- [x] Agent token in Windows Credential Manager
- [x] Checkpoint advances only on HTTP 200
- [x] Log redaction for tokens, XML, financial fields
- [x] Access token in memory only (web)
- [x] Refresh cookie never touched by JS (web)
- [x] Generic pairing/login error messages
- [x] Code signing config scaffold (release builds)

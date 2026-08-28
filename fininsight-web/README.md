# FinInsight Web Dashboard

React frontend for the FinInsight accounting SaaS. Implements secure login integration with the existing Express/Prisma backend — in-memory JWT storage, HttpOnly refresh cookies, protected routes (UX only), pairing code generation, team management, and connection switching.

## Security Model

- **Access token**: stored in memory only (`tokenStore.ts`) — never `localStorage` or `sessionStorage`
- **Refresh token**: HttpOnly cookie only — JavaScript never reads or attaches it
- **All requests**: `credentials: "include"` for cookie + manual `Authorization: Bearer` header
- **Generic login errors**: same message for wrong password, unknown email, etc.
- **`organizationId`**: decoded from JWT for display only — never sent as a trusted value
- **Protected routes**: UX convenience; server enforces all authorization

## Setup

```powershell
cd fininsight-web
npm install
npm run dev
```

The dev server proxies `/api` to `http://localhost:3000` (override with `VITE_API_BASE`).

## Features

| Feature | Route / Component |
|---|---|
| Login | `/login` → `LoginPage` |
| Silent refresh on load | `AuthProvider` |
| Token refresh interceptor | `api/client.ts` |
| Protected dashboard | `ProtectedRoute` |
| Pairing code (OWNER/ADMIN) | `PairingPanel` |
| Sync Now flag (dashboard) | `PairingPanel` → agent polls heartbeat |
| Team management | `TeamManagement` |
| Connection switcher | `ConnectionSwitcher` |

## Tests

```powershell
npm test
```

Covers: no token in storage, role-based UI visibility, generic login errors, single refresh retry on 401.

## API Endpoints Used

| Method | Path |
|---|---|
| POST | `/api/v1/auth/login` |
| POST | `/api/v1/auth/refresh` |
| GET | `/api/v1/connections` |
| POST | `/api/v1/agent/pairing/initiate` |
| POST | `/api/v1/connections/:id/sync-request` |
| GET | `/api/v1/organization/members` |
| POST | `/api/v1/organization/members/invite` |

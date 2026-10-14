import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import type { ReactElement } from "react";
import { MemoryRouter } from "react-router-dom";
import { AuthProvider } from "../auth/AuthContext";
import { PairingPanel } from "../components/PairingPanel";
import { TeamManagement } from "../components/TeamManagement";
import { ConnectionProvider } from "../context/ConnectionContext";
import { apiFetch } from "../api/client";
import {
  clearAuthState,
  getAccessToken,
  setAccessToken,
} from "../auth/tokenStore";

vi.mock("../api/client", () => ({
  apiFetch: vi.fn(),
  login: vi.fn(),
  silentRefreshOnLoad: vi.fn().mockResolvedValue(null),
  GENERIC_LOGIN_ERROR: "Invalid email or password",
  LOCKOUT_MESSAGE: "Your account is temporarily locked. Please try again later.",
}));

function renderWithAuth(ui: ReactElement, role = "MEMBER") {
  const token = btoa(JSON.stringify({ alg: "none" })) +
    "." +
    btoa(
      JSON.stringify({
        sub: "user-1",
        email: "test@example.com",
        organizationId: "org-1",
        role,
      })
    ) +
    ".sig";
  setAccessToken(token);

  return render(
    <MemoryRouter>
      <AuthProvider>
        <ConnectionProvider>{ui}</ConnectionProvider>
      </AuthProvider>
    </MemoryRouter>
  );
}

describe("token storage", () => {
  beforeEach(() => {
    cleanup();
    clearAuthState();
    localStorage.clear();
    sessionStorage.clear();
  });

  afterEach(() => {
    cleanup();
  });

  it("never stores access token in localStorage or sessionStorage", () => {
    setAccessToken("test-access-token-abc");
    expect(getAccessToken()).toBe("test-access-token-abc");
    expect(localStorage.getItem("accessToken")).toBeNull();
    expect(sessionStorage.getItem("accessToken")).toBeNull();
    expect(localStorage.length).toBe(0);
    expect(sessionStorage.length).toBe(0);
  });
});

describe("role-based UI and pairing", () => {
  beforeEach(() => {
    cleanup();
    clearAuthState();
    vi.clearAllMocks();
  });

  afterEach(() => {
    cleanup();
  });

  it("MEMBER does not see Tally Agent Pairing panel", async () => {
    renderWithAuth(<PairingPanel />, "MEMBER");
    expect(screen.queryByText("Tally Agent Pairing")).not.toBeInTheDocument();
  });

  it("ADMIN sees Tally Agent Pairing with device authorization instructions", async () => {
    renderWithAuth(<PairingPanel />, "ADMIN");
    expect(screen.getByText("Tally Agent Pairing")).toBeInTheDocument();
    expect(screen.getByText(/device authorization flow/i)).toBeInTheDocument();
    expect(screen.getByText(/\/device\?user_code=XXXX/i)).toBeInTheDocument();
  });

  it("MEMBER does not see add member form", async () => {
    renderWithAuth(<TeamManagement />, "MEMBER");
    expect(screen.queryByRole("heading", { name: "Add Member" })).not.toBeInTheDocument();
  });

  it("ADMIN sees add member form but not OWNER role unless user is OWNER", async () => {
    renderWithAuth(<TeamManagement />, "ADMIN");
    expect(screen.getByRole("heading", { name: "Add Member" })).toBeInTheDocument();
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).not.toContain("OWNER");
  });

  it("OWNER sees OWNER in role selector", async () => {
    renderWithAuth(<TeamManagement />, "OWNER");
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toContain("OWNER");
  });

  it("unwraps member list from { data: [...] } structure", async () => {
    vi.mocked(apiFetch).mockImplementation(async (url) => {
      if (url === "/api/v1/organization/members") {
        return {
          data: [
            { id: "m-1", email: "alice@example.com", name: "Alice", role: "ADMIN" },
            { id: "m-2", email: "bob@example.com", name: "Bob", role: "MEMBER" },
          ],
        };
      }
      return [];
    });

    renderWithAuth(<TeamManagement />, "ADMIN");

    await waitFor(() => {
      expect(screen.getByText(/alice@example.com/i)).toBeInTheDocument();
      expect(screen.getByText(/bob@example.com/i)).toBeInTheDocument();
    });
  });

  it("submits name, email, and role to /api/v1/organization/members and displays temporary password", async () => {
    vi.mocked(apiFetch).mockImplementation(async (url, init) => {
      if (url === "/api/v1/organization/members" && init?.method === "POST") {
        return { tempPassword: "TempPassword123!" };
      }
      if (url === "/api/v1/organization/members") {
        return { data: [] };
      }
      return [];
    });

    renderWithAuth(<TeamManagement />, "ADMIN");

    fireEvent.change(screen.getByPlaceholderText("Full name"), {
      target: { value: "Charlie" },
    });
    fireEvent.change(screen.getByPlaceholderText("Email address"), {
      target: { value: "charlie@example.com" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Add Member" }));

    await waitFor(() => {
      expect(apiFetch).toHaveBeenCalledWith("/api/v1/organization/members", {
        method: "POST",
        body: JSON.stringify({ name: "Charlie", email: "charlie@example.com", role: "MEMBER" }),
      });
    });

    await waitFor(() => {
      expect(screen.getByText("TempPassword123!")).toBeInTheDocument();
    });
  });

  it("calls /api/v1/connections/:id/sync when Sync Now is clicked", async () => {
    vi.mocked(apiFetch).mockImplementation(async (url, init) => {
      if (url === "/api/v1/connections") {
        return [
          { id: "conn-abc", companyName: "Test Co", status: "ACTIVE", lastSyncAt: null },
        ];
      }
      if (url === "/api/v1/connections/conn-abc/sync" && init?.method === "POST") {
        return {
          message: "Sync requested",
          connection_id: "conn-abc",
          request_id: "req-1",
        };
      }
      return [];
    });

    renderWithAuth(<PairingPanel />, "ADMIN");

    await waitFor(() => {
      expect(screen.getByRole("button", { name: /Sync Now/i })).toBeInTheDocument();
    });

    fireEvent.click(screen.getByRole("button", { name: /Sync Now/i }));

    await waitFor(() => {
      expect(apiFetch).toHaveBeenCalledWith("/api/v1/connections/conn-abc/sync", {
        method: "POST",
      });
    });
  });
});

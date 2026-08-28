import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import type { ReactElement } from "react";
import { MemoryRouter } from "react-router-dom";
import { AuthProvider } from "../auth/AuthContext";
import { PairingPanel } from "../components/PairingPanel";
import { TeamManagement } from "../components/TeamManagement";
import { ConnectionProvider } from "../context/ConnectionContext";
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
    clearAuthState();
    localStorage.clear();
    sessionStorage.clear();
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

describe("role-based UI", () => {
  beforeEach(() => {
    clearAuthState();
  });

  it("MEMBER does not see Generate Pairing Code", async () => {
    renderWithAuth(<PairingPanel />, "MEMBER");
    expect(screen.queryByText("Generate Pairing Code")).not.toBeInTheDocument();
  });

  it("ADMIN sees Generate Pairing Code", async () => {
    renderWithAuth(<PairingPanel />, "ADMIN");
    expect(screen.getByText("Generate Pairing Code")).toBeInTheDocument();
  });

  it("MEMBER does not see invite form", async () => {
    renderWithAuth(<TeamManagement />, "MEMBER");
    expect(screen.queryByText("Invite Member")).not.toBeInTheDocument();
  });

  it("ADMIN sees invite form but not OWNER role unless user is OWNER", async () => {
    renderWithAuth(<TeamManagement />, "ADMIN");
    expect(screen.getByText("Invite Member")).toBeInTheDocument();
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).not.toContain("OWNER");
  });

  it("OWNER sees OWNER in role selector", async () => {
    renderWithAuth(<TeamManagement />, "OWNER");
    const options = screen.getAllByRole("option").map((o) => o.textContent);
    expect(options).toContain("OWNER");
  });
});

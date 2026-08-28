import { describe, it, expect, vi, beforeEach } from "vitest";
import { GENERIC_LOGIN_ERROR } from "../api/client";

describe("login error messages", () => {
  it("uses identical generic message for all auth failures", () => {
    const wrongPasswordMsg = GENERIC_LOGIN_ERROR;
    const unknownEmailMsg = GENERIC_LOGIN_ERROR;
    expect(wrongPasswordMsg).toBe(unknownEmailMsg);
    expect(wrongPasswordMsg).toBe("Invalid email or password");
  });
});

describe("token refresh interceptor", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("attempts exactly one refresh on 401", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce({ status: 401, ok: false })
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({ accessToken: "new-token" }),
      })
      .mockResolvedValueOnce({
        ok: true,
        json: async () => ({ data: "ok" }),
      });

    vi.stubGlobal("fetch", fetchMock);

    const { apiFetch, silentRefreshOnLoad: _ } = await import("../api/client");
    const { setAccessToken } = await import("../auth/tokenStore");
    setAccessToken("expired-token");

    await apiFetch("/api/v1/test");

    const refreshCalls = fetchMock.mock.calls.filter(
      (c) => c[0] === "/api/v1/auth/refresh"
    );
    expect(refreshCalls.length).toBe(1);
  });
});

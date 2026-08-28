import { getAccessToken, setAccessToken } from "../auth/tokenStore";

const GENERIC_LOGIN_ERROR = "Invalid email or password";
const LOCKOUT_MESSAGE =
  "Your account is temporarily locked. Please try again later.";

export class ApiClientError extends Error {
  constructor(
    message: string,
    public status: number
  ) {
    super(message);
  }
}

async function parseJson<T>(response: Response): Promise<T> {
  if (!response.ok) {
    throw new ApiClientError(`Request failed (${response.status})`, response.status);
  }
  return response.json() as Promise<T>;
}

/**
 * Authenticated fetch wrapper.
 * - Attaches in-memory Bearer token
 * - Sends refresh cookie via credentials: "include"
 * - Retries once after silent refresh on 401
 */
export async function apiFetch<T>(
  path: string,
  options: RequestInit = {},
  retried = false
): Promise<T> {
  const headers = new Headers(options.headers);
  headers.set("Content-Type", "application/json");

  const token = getAccessToken();
  if (token) {
    headers.set("Authorization", `Bearer ${token}`);
  }

  const response = await fetch(path, {
    ...options,
    headers,
    credentials: "include",
  });

  if (response.status === 401 && !retried) {
    const refreshed = await attemptRefresh();
    if (refreshed) {
      return apiFetch<T>(path, options, true);
    }
    clearAuthAndRedirect();
    throw new ApiClientError("Session expired", 401);
  }

  return parseJson<T>(response);
}

async function attemptRefresh(): Promise<boolean> {
  try {
    const response = await fetch("/api/v1/auth/refresh", {
      method: "POST",
      credentials: "include",
    });
    if (!response.ok) return false;
    const data = (await response.json()) as { accessToken: string };
    setAccessToken(data.accessToken);
    return true;
  } catch {
    return false;
  }
}

function clearAuthAndRedirect(): void {
  setAccessToken(null);
  if (window.location.pathname !== "/login") {
    window.location.href = "/login";
  }
}

export async function login(
  email: string,
  password: string
): Promise<{ accessToken: string }> {
  const response = await fetch("/api/v1/auth/login", {
    method: "POST",
    credentials: "include",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ email, password }),
  });

  if (response.status === 429) {
    throw new ApiClientError(LOCKOUT_MESSAGE, 429);
  }

  if (!response.ok) {
    throw new ApiClientError(GENERIC_LOGIN_ERROR, response.status);
  }

  return response.json() as Promise<{ accessToken: string }>;
}

export async function silentRefreshOnLoad(): Promise<string | null> {
  try {
    const response = await fetch("/api/v1/auth/refresh", {
      method: "POST",
      credentials: "include",
    });
    if (!response.ok) return null;
    const data = (await response.json()) as { accessToken: string };
    setAccessToken(data.accessToken);
    return data.accessToken;
  } catch {
    return null;
  }
}

export { GENERIC_LOGIN_ERROR, LOCKOUT_MESSAGE };

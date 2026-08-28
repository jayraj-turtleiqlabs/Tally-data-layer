import type { User } from "../types/auth";

/** In-memory only — never persisted to localStorage/sessionStorage. */
let accessToken: string | null = null;

export function getAccessToken(): string | null {
  return accessToken;
}

export function setAccessToken(token: string | null): void {
  accessToken = token;
}

export function decodeJwtClaims(token: string): User | null {
  try {
    const payload = token.split(".")[1];
    const decoded = JSON.parse(atob(payload.replace(/-/g, "+").replace(/_/g, "/")));
    return {
      id: decoded.sub ?? decoded.userId,
      email: decoded.email,
      organizationId: decoded.organizationId,
      role: decoded.role,
    };
  } catch {
    return null;
  }
}

export function clearAuthState(): void {
  accessToken = null;
}

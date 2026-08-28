import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { login as apiLogin, silentRefreshOnLoad } from "../api/client";
import {
  clearAuthState,
  decodeJwtClaims,
  getAccessToken,
  setAccessToken,
} from "../auth/tokenStore";
import type { User, UserRole } from "../types/auth";

interface AuthContextValue {
  isAuthenticated: boolean;
  isLoading: boolean;
  user: User | null;
  login: (email: string, password: string) => Promise<void>;
  logout: () => void;
  hasRole: (...roles: UserRole[]) => boolean;
}

const AuthContext = createContext<AuthContextValue | null>(null);

export function AuthProvider({ children }: { children: ReactNode }) {
  const [user, setUser] = useState<User | null>(() => {
    const token = getAccessToken();
    return token ? decodeJwtClaims(token) : null;
  });
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    silentRefreshOnLoad().then((token) => {
      if (token) {
        setUser(decodeJwtClaims(token));
      }
      setIsLoading(false);
    });
  }, []);

  const login = useCallback(async (email: string, password: string) => {
    const { accessToken } = await apiLogin(email, password);
    setAccessToken(accessToken);
    setUser(decodeJwtClaims(accessToken));
  }, []);

  const logout = useCallback(() => {
    clearAuthState();
    setUser(null);
  }, []);

  const hasRole = useCallback(
    (...roles: UserRole[]) => {
      if (!user) return false;
      return roles.includes(user.role);
    },
    [user]
  );

  const value = useMemo(
    () => ({
      isAuthenticated: !!getAccessToken() && !!user,
      isLoading,
      user,
      login,
      logout,
      hasRole,
    }),
    [user, isLoading, login, logout, hasRole]
  );

  return <AuthContext.Provider value={value}>{children}</AuthContext.Provider>;
}

export function useAuth(): AuthContextValue {
  const ctx = useContext(AuthContext);
  if (!ctx) throw new Error("useAuth must be used within AuthProvider");
  return ctx;
}

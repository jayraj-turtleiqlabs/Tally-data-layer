import { Navigate, Outlet } from "react-router-dom";
import { useAuth } from "../auth/AuthContext";

/**
 * UX convenience only — NOT a security boundary.
 * All authorization is enforced server-side on every API request.
 * This wrapper simply redirects unauthenticated users to the login page.
 */
export function ProtectedRoute() {
  const { isAuthenticated, isLoading } = useAuth();

  if (isLoading) {
    return <div className="loading">Loading…</div>;
  }

  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }

  return <Outlet />;
}

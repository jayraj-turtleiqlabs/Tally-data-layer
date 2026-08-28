import { ConnectionSwitcher } from "../components/ConnectionSwitcher";
import { PairingPanel } from "../components/PairingPanel";
import { TeamManagement } from "../components/TeamManagement";
import { useAuth } from "../auth/AuthContext";

export function DashboardPage() {
  const { user, logout } = useAuth();

  return (
    <div className="dashboard">
      <header className="dashboard-header">
        <div>
          <h1>FinInsight Dashboard</h1>
          <p className="user-info">
            {user?.email} · {user?.role}
          </p>
        </div>
        <div className="header-actions">
          <ConnectionSwitcher />
          <button onClick={logout} className="secondary">
            Sign out
          </button>
        </div>
      </header>

      <main className="dashboard-main">
        <PairingPanel />
        <TeamManagement />
      </main>
    </div>
  );
}

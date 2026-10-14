import { useCallback, useState } from "react";
import { apiFetch } from "../api/client";
import { useAuth } from "../auth/AuthContext";
import { useConnections } from "../context/ConnectionContext";

export function PairingPanel() {
  const { hasRole } = useAuth();
  const { activeConnectionId } = useConnections();
  const [syncRequested, setSyncRequested] = useState(false);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const canManage = hasRole("OWNER", "ADMIN");

  const requestSync = useCallback(async () => {
    if (!activeConnectionId) return;
    setLoading(true);
    setError("");
    setSyncRequested(false);
    try {
      await apiFetch(`/api/v1/connections/${activeConnectionId}/sync`, {
        method: "POST",
      });
      setSyncRequested(true);
    } catch {
      setError("Unable to request sync. Please try again.");
    } finally {
      setLoading(false);
    }
  }, [activeConnectionId]);

  if (!canManage) return null;

  return (
    <section className="panel">
      <h2>Tally Agent Pairing</h2>
      <div className="device-pairing-instructions">
        <p>
          Pairing is performed via the secure device authorization flow:
        </p>
        <ol>
          <li>Open the FinInsight Tally Agent on your Windows machine running Tally.</li>
          <li>Click <strong>Connect Company</strong> to initiate pairing and obtain a <strong>user code</strong>.</li>
          <li>
            An OWNER or ADMIN must approve the connection on the gateway at{" "}
            <code>/device?user_code=XXXX</code> (served by the gateway).
          </li>
        </ol>
      </div>

      {activeConnectionId && (
        <button className="secondary" onClick={requestSync} disabled={loading}>
          {loading ? "Requesting…" : "Sync Now (via agent poll)"}
        </button>
      )}

      {syncRequested && (
        <p className="success-message">Sync requested. The agent will sync on its next heartbeat.</p>
      )}

      {error && <p className="error">{error}</p>}
    </section>
  );
}

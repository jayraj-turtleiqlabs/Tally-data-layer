import { useCallback, useEffect, useState } from "react";
import { apiFetch } from "../api/client";
import { useAuth } from "../auth/AuthContext";
import { useConnections } from "../context/ConnectionContext";
import type { PairingInitResponse } from "../types/auth";

export function PairingPanel() {
  const { hasRole } = useAuth();
  const { refreshConnections, activeConnectionId } = useConnections();
  const [pairing, setPairing] = useState<PairingInitResponse | null>(null);
  const [remainingMs, setRemainingMs] = useState(0);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const canGenerate = hasRole("OWNER", "ADMIN");

  useEffect(() => {
    if (!pairing) return;
    const expiresAt = new Date(pairing.expiresAt).getTime();
    const tick = () => setRemainingMs(Math.max(0, expiresAt - Date.now()));
    tick();
    const id = setInterval(tick, 1000);
    return () => clearInterval(id);
  }, [pairing]);

  useEffect(() => {
    if (!pairing) return;
    const poll = setInterval(() => refreshConnections(), 5000);
    return () => clearInterval(poll);
  }, [pairing, refreshConnections]);

  const initiatePairing = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const data = await apiFetch<PairingInitResponse>(
        "/api/v1/agent/pairing/initiate",
        { method: "POST" }
      );
      setPairing(data);
    } catch {
      setError("Unable to generate pairing code. Please try again.");
    } finally {
      setLoading(false);
    }
  }, []);

  const requestSync = useCallback(async () => {
    if (!activeConnectionId) return;
    try {
      await apiFetch(`/api/v1/connections/${activeConnectionId}/sync-request`, {
        method: "POST",
      });
    } catch {
      setError("Unable to request sync. Please try again.");
    }
  }, [activeConnectionId]);

  if (!canGenerate) return null;

  const minutes = Math.floor(remainingMs / 60000);
  const seconds = Math.floor((remainingMs % 60000) / 1000);

  return (
    <section className="panel">
      <h2>Tally Agent Pairing</h2>
      {!pairing ? (
        <button onClick={initiatePairing} disabled={loading}>
          {loading ? "Generating…" : "Generate Pairing Code"}
        </button>
      ) : (
        <div className="pairing-code-display">
          <p className="code">{pairing.pairingCode}</p>
          <p className="expiry">
            Expires in {minutes}:{seconds.toString().padStart(2, "0")}
          </p>
          <p className="hint">
            Enter this code in the FinInsight Tally Agent on the machine running Tally.
          </p>
        </div>
      )}

      {activeConnectionId && (
        <button className="secondary" onClick={requestSync}>
          Sync Now (via agent poll)
        </button>
      )}

      {error && <p className="error">{error}</p>}
    </section>
  );
}

import { useConnections } from "../context/ConnectionContext";

export function ConnectionSwitcher() {
  const { connections, activeConnectionId, setActiveConnectionId, loading } =
    useConnections();

  if (loading) return <span className="connection-switcher">Loading…</span>;

  return (
    <label className="connection-switcher">
      Company:{" "}
      <select
        value={activeConnectionId ?? ""}
        onChange={(e) => setActiveConnectionId(e.target.value)}
      >
        {connections.map((c) => (
          <option key={c.id} value={c.id}>
            {c.companyName} ({c.status})
          </option>
        ))}
      </select>
    </label>
  );
}

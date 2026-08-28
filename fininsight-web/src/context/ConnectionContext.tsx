import { createContext, useCallback, useContext, useEffect, useState, type ReactNode } from "react";
import { apiFetch } from "../api/client";
import type { Connection } from "../types/auth";

interface ConnectionContextValue {
  connections: Connection[];
  activeConnectionId: string | null;
  setActiveConnectionId: (id: string) => void;
  refreshConnections: () => Promise<void>;
  loading: boolean;
}

const ConnectionContext = createContext<ConnectionContextValue | null>(null);

export function ConnectionProvider({ children }: { children: ReactNode }) {
  const [connections, setConnections] = useState<Connection[]>([]);
  const [activeConnectionId, setActiveConnectionId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);

  const refreshConnections = useCallback(async () => {
    setLoading(true);
    try {
      const data = await apiFetch<Connection[]>("/api/v1/connections");
      const nextConnections = Array.isArray(data) ? data : [];
      setConnections(nextConnections);
      if (!activeConnectionId && nextConnections.length > 0) {
        setActiveConnectionId(nextConnections[0].id);
      }
    } finally {
      setLoading(false);
    }
  }, [activeConnectionId]);

  useEffect(() => {
    refreshConnections();
  }, [refreshConnections]);

  return (
    <ConnectionContext.Provider
      value={{
        connections,
        activeConnectionId,
        setActiveConnectionId,
        refreshConnections,
        loading,
      }}
    >
      {children}
    </ConnectionContext.Provider>
  );
}

export function useConnections(): ConnectionContextValue {
  const ctx = useContext(ConnectionContext);
  if (!ctx) throw new Error("useConnections must be used within ConnectionProvider");
  return ctx;
}

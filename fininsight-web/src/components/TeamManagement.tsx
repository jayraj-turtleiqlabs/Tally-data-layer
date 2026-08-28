import { useCallback, useEffect, useState, type FormEvent } from "react";
import { apiFetch } from "../api/client";
import { useAuth } from "../auth/AuthContext";
import type { OrgMember, UserRole } from "../types/auth";

const INVITE_ROLES: UserRole[] = ["ADMIN", "MEMBER", "READ_ONLY"];

export function TeamManagement() {
  const { user, hasRole } = useAuth();
  const [members, setMembers] = useState<OrgMember[]>([]);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<UserRole>("MEMBER");
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  const canInvite = hasRole("OWNER", "ADMIN");
  const isOwner = user?.role === "OWNER";

  const availableRoles: UserRole[] = isOwner
    ? ["OWNER", ...INVITE_ROLES]
    : INVITE_ROLES;

  const loadMembers = useCallback(async () => {
    try {
      const data = await apiFetch<OrgMember[]>("/api/v1/organization/members");
      setMembers(Array.isArray(data) ? data : []);
    } catch {
      setError("Unable to load team members.");
    }
  }, []);

  useEffect(() => {
    loadMembers();
  }, [loadMembers]);

  async function handleInvite(e: FormEvent) {
    e.preventDefault();
    if (!canInvite) return;
    setLoading(true);
    setError("");
    try {
      await apiFetch("/api/v1/organization/members/invite", {
        method: "POST",
        body: JSON.stringify({ email, role }),
      });
      setEmail("");
      setRole("MEMBER");
      await loadMembers();
    } catch {
      setError("Unable to send invite. Please try again.");
    } finally {
      setLoading(false);
    }
  }

  return (
    <section className="panel">
      <h2>Team</h2>
      <ul className="member-list">
        {members.map((m) => (
          <li key={m.id}>
            <span>{m.email}</span>
            <span className="role-badge">{m.role}</span>
          </li>
        ))}
      </ul>

      {canInvite && (
        <form onSubmit={handleInvite} className="invite-form">
          <h3>Invite Member</h3>
          <input
            type="email"
            placeholder="Email address"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
            required
          />
          <select
            value={role}
            onChange={(e) => setRole(e.target.value as UserRole)}
          >
            {availableRoles.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
          <button type="submit" disabled={loading}>
            {loading ? "Sending…" : "Send Invite"}
          </button>
        </form>
      )}

      {error && <p className="error">{error}</p>}
    </section>
  );
}

import { useCallback, useEffect, useState, type FormEvent } from "react";
import { apiFetch } from "../api/client";
import { useAuth } from "../auth/AuthContext";
import type { OrgMember, UserRole } from "../types/auth";

const MEMBER_ROLES: UserRole[] = ["ADMIN", "MEMBER", "READ_ONLY"];

interface TempPasswordInfo {
  email: string;
  password: string;
}

export function TeamManagement() {
  const { user, hasRole } = useAuth();
  const [members, setMembers] = useState<OrgMember[]>([]);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [role, setRole] = useState<UserRole>("MEMBER");
  const [tempPasswordInfo, setTempPasswordInfo] = useState<TempPasswordInfo | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);

  const canAddMember = hasRole("OWNER", "ADMIN");
  const isOwner = user?.role === "OWNER";

  const availableRoles: UserRole[] = isOwner
    ? ["OWNER", ...MEMBER_ROLES]
    : MEMBER_ROLES;

  const loadMembers = useCallback(async () => {
    try {
      const res = await apiFetch<{ data?: OrgMember[] } | OrgMember[]>("/api/v1/organization/members");
      if (Array.isArray(res)) {
        setMembers(res);
      } else if (res && Array.isArray(res.data)) {
        setMembers(res.data);
      } else {
        setMembers([]);
      }
    } catch {
      setError("Unable to load team members.");
    }
  }, []);

  useEffect(() => {
    loadMembers();
  }, [loadMembers]);

  async function handleAddMember(e: FormEvent) {
    e.preventDefault();
    if (!canAddMember) return;
    setLoading(true);
    setError("");
    setTempPasswordInfo(null);
    try {
      const res = await apiFetch<{
        tempPassword?: string;
        temporaryPassword?: string;
        data?: { tempPassword?: string; temporaryPassword?: string };
      }>("/api/v1/organization/members", {
        method: "POST",
        body: JSON.stringify({ name: name.trim(), email: email.trim(), role }),
      });
      const returnedPassword =
        res?.tempPassword ||
        res?.temporaryPassword ||
        res?.data?.tempPassword ||
        res?.data?.temporaryPassword;
      if (returnedPassword) {
        setTempPasswordInfo({ email: email.trim(), password: returnedPassword });
      }
      setName("");
      setEmail("");
      setRole("MEMBER");
      await loadMembers();
    } catch {
      setError("Unable to add member. Please try again.");
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
            <span>{m.name ? `${m.name} (${m.email})` : m.email}</span>
            <span className="role-badge">{m.role}</span>
          </li>
        ))}
      </ul>

      {tempPasswordInfo && (
        <div className="temp-password-banner" role="alert">
          <p>
            Temporary password for <strong>{tempPasswordInfo.email}</strong>:{" "}
            <code>{tempPasswordInfo.password}</code>
          </p>
          <p className="hint">This temporary password is shown once. Please record it safely.</p>
        </div>
      )}

      {canAddMember && (
        <form onSubmit={handleAddMember} className="invite-form add-member-form">
          <h3>Add Member</h3>
          <input
            type="text"
            placeholder="Full name"
            value={name}
            onChange={(e) => setName(e.target.value)}
            required
            minLength={2}
            maxLength={100}
          />
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
            {loading ? "Adding…" : "Add Member"}
          </button>
        </form>
      )}

      {error && <p className="error">{error}</p>}
    </section>
  );
}

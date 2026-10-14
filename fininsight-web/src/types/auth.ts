export type UserRole = "OWNER" | "ADMIN" | "MEMBER" | "READ_ONLY";

export interface User {
  id: string;
  email: string;
  organizationId: string;
  role: UserRole;
}

export interface AuthTokens {
  accessToken: string;
}

export interface Connection {
  id: string;
  companyName: string;
  status: "PENDING" | "ACTIVE" | "DISCONNECTED" | "ERROR";
  lastSyncAt: string | null;
}

export interface OrgMember {
  id: string;
  userId?: string;
  name?: string;
  email: string;
  role: UserRole;
  createdAt?: string;
}

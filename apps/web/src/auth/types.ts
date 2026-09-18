export type MembershipRole =
  | "tenant_admin"
  | "member"
  | "viewer"
  | "operator_agent"
  | "operator_admin"
  | "resource_admin";

export interface SessionUser {
  id: string;
  login_name: string;
  display_name: string;
}

export interface SessionOperator {
  id: string;
  slug: string;
  display_name: string;
}

export interface TenantMembership {
  tenant_id: string;
  tenant_slug: string;
  tenant_display_name: string;
  role: MembershipRole;
}

export interface AuthSession {
  user: SessionUser;
  operator: SessionOperator;
  memberships: TenantMembership[];
  expires_at: string;
  csrf_token: string;
}

export interface QueryScope {
  userId: string;
  operatorId: string;
  tenantId: string;
  projectId?: string;
}

export function queryScopeFor(
  session: AuthSession,
  tenantId: string,
  projectId?: string,
): QueryScope {
  return {
    userId: session.user.id,
    operatorId: session.operator.id,
    tenantId,
    projectId,
  };
}

export function membershipForTenant(
  session: AuthSession | undefined,
  tenantId: string | undefined,
): TenantMembership | undefined {
  if (!session || !tenantId) return undefined;
  return session.memberships.find(
    (membership) => membership.tenant_id === tenantId,
  );
}

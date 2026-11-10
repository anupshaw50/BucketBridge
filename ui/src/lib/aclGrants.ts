// Rules and labels for the ACL grant editor of buckets and objects. The rules are those of
// bucketbridge_core::objectmeta::validate_grants; the mock backend applies the same ones, and the
// real backend re-checks everything.

import type { AclGrant, AclPermission, GranteeType } from "../api/types";
import { tr } from "../i18n";

export const ALL_USERS_URI = "http://acs.amazonaws.com/groups/global/AllUsers";
export const AUTHENTICATED_USERS_URI = "http://acs.amazonaws.com/groups/global/AuthenticatedUsers";
export const LOG_DELIVERY_URI = "http://acs.amazonaws.com/groups/s3/LogDelivery";

const MAX_GRANTEE_LENGTH = 256;

export const ACL_GROUPS: readonly { uri: string; label: string }[] = [
  { uri: ALL_USERS_URI, label: tr("Everyone (AllUsers)") },
  { uri: AUTHENTICATED_USERS_URI, label: tr("Any AWS account (AuthenticatedUsers)") },
  { uri: LOG_DELIVERY_URI, label: tr("S3 log delivery (LogDelivery)") },
];

export const ACL_PERMISSIONS: readonly { value: AclPermission; label: string }[] = [
  { value: "READ", label: tr("READ: list the bucket or read the object") },
  { value: "WRITE", label: tr("WRITE: create, replace and delete objects (buckets only)") },
  { value: "READ_ACP", label: tr("READ_ACP: read the ACL") },
  { value: "WRITE_ACP", label: tr("WRITE_ACP: change the ACL") },
  { value: "FULL_CONTROL", label: tr("FULL_CONTROL: all of the above") },
];

export const GRANTEE_KINDS: readonly { value: GranteeType; label: string }[] = [
  { value: "canonical_user", label: tr("AWS account ID") },
  { value: "email", label: tr("E-mail address") },
  { value: "group", label: tr("Predefined group") },
];

/** The group's label, or its URI for one this tool does not know. */
export function groupLabel(uri: string): string {
  return ACL_GROUPS.find((group) => group.uri === uri)?.label ?? uri;
}

/** The grantee as a table shows it. */
export function granteeText(grant: AclGrant): string {
  return grant.grantee_type === "group" ? groupLabel(grant.grantee) : grant.grantee;
}

/** A grant that reaches people outside the account. */
export function isPublicGrant(grant: AclGrant): boolean {
  return grant.grantee_type === "group" && (grant.grantee === ALL_USERS_URI || grant.grantee === AUTHENTICATED_USERS_URI);
}

export function grantId(grant: AclGrant): string {
  return `${grant.grantee_type}\u0000${grant.grantee}\u0000${grant.permission}`;
}

export function sameGrants(a: readonly AclGrant[], b: readonly AclGrant[]): boolean {
  const left = a.map(grantId).sort();
  const right = b.map(grantId).sort();
  return left.length === right.length && left.every((id, index) => id === right[index]);
}

/** The grantee that is typed (not chosen): `null` when it is valid for its kind. */
export function validateGrantee(kind: GranteeType, who: string): string | null {
  if (who.length === 0) {
    return kind === "email" ? tr("Enter an e-mail address.") : tr("Enter an account ID.");
  }
  if (who !== who.trim() || /[\s\p{Cc}]/u.test(who)) {
    return tr("The grantee cannot contain spaces.");
  }
  if ([...who].length > MAX_GRANTEE_LENGTH) {
    return tr("The grantee is longer than {n} characters.", { n: MAX_GRANTEE_LENGTH });
  }
  if (kind === "email") {
    const [name, host] = who.split("@", 2);
    if (name === undefined || host === undefined || name.length === 0 || !host.includes(".")) {
      return tr("This is not an e-mail address.");
    }
  }
  return null;
}

/** A whole grant list: not empty, valid grantees, no grant twice. `null` when it is valid. */
export function validateGrantList(grants: readonly AclGrant[]): string | null {
  if (grants.length === 0) {
    return tr("An ACL needs at least one grant; the owner usually keeps FULL_CONTROL.");
  }
  const seen = new Set<string>();
  for (const grant of grants) {
    if (grant.grantee_type === "group") {
      if (!ACL_GROUPS.some((group) => group.uri === grant.grantee)) {
        return tr("\"{who}\" is not a predefined group.", { who: grant.grantee });
      }
    } else {
      const problem = validateGrantee(grant.grantee_type, grant.grantee);
      if (problem !== null) {
        return problem;
      }
    }
    const id = grantId(grant);
    if (seen.has(id)) {
      return tr("The grant {permission} to \"{who}\" is listed twice.", { permission: grant.permission, who: grant.grantee });
    }
    seen.add(id);
  }
  return null;
}

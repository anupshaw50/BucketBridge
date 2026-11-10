import { ActionIcon, Alert, Button, Group, Select, Stack, Table, Text, TextInput } from "@mantine/core";
import { IconAlertCircle, IconInfoCircle, IconPlus, IconTrash } from "@tabler/icons-react";
import { useState } from "react";
import type { AclGrant, AclPermission, GranteeType } from "../../api/types";
import { tr } from "../../i18n";
import {
  ACL_GROUPS,
  ACL_PERMISSIONS,
  ALL_USERS_URI,
  GRANTEE_KINDS,
  grantId,
  granteeText,
  isPublicGrant,
  sameGrants,
  validateGrantee,
  validateGrantList,
} from "../../lib/aclGrants";
import { ActionError } from "../bucketcfg/common";
import { useAction } from "../bucketcfg/useAction";

function isKind(value: string | null): value is GranteeType {
  return value === "canonical_user" || value === "email" || value === "group";
}

function isPermission(value: string | null): value is AclPermission {
  return ACL_PERMISSIONS.some((item) => item.value === value);
}

/**
 * Lists the grants of a bucket or an object and edits them one by one: add a grant for an account ID,
 * an e-mail address or a predefined group with one permission, remove a grant, then save the whole
 * list. A bucket with ACLs disabled shows the grants and explains why they cannot be changed.
 */
export function AclGrantsEditor({
  scope,
  grants,
  disabled,
  onSave,
}: {
  scope: "bucket" | "object";
  /** The grants as they are now; the editor starts over when the list changes. */
  grants: readonly AclGrant[];
  /** The bucket has ACLs disabled (Object Ownership BucketOwnerEnforced). */
  disabled: boolean;
  /** Replaces every grant; a failure is shown in the editor. */
  onSave: (grants: AclGrant[]) => Promise<void>;
}) {
  const [draft, setDraft] = useState<AclGrant[]>(() => grants.map((grant) => ({ ...grant })));
  const [kind, setKind] = useState<GranteeType>("canonical_user");
  const [who, setWho] = useState("");
  const [group, setGroup] = useState<string>(ALL_USERS_URI);
  const [permission, setPermission] = useState<AclPermission>("READ");
  const [touched, setTouched] = useState(false);
  const action = useAction();

  const permissions = ACL_PERMISSIONS.filter((item) => scope === "bucket" || item.value !== "WRITE");
  const grantee = kind === "group" ? group : who.trim();
  const granteeProblem = kind === "group" ? null : validateGrantee(kind, grantee);
  const candidate: AclGrant = { grantee_type: kind, grantee, permission };
  const duplicate = draft.some((item) => grantId(item) === grantId(candidate));
  const changed = !sameGrants(draft, grants);
  const listProblem = changed ? validateGrantList(draft) : null;
  const addedPublic = draft.some((item) => isPublicGrant(item) && !grants.some((saved) => grantId(saved) === grantId(item)));

  const add = () => {
    setTouched(true);
    if (granteeProblem !== null || duplicate) {
      return;
    }
    setDraft((current) => [...current, candidate]);
    setWho("");
    setTouched(false);
  };
  const save = () => action.run(() => onSave(draft));

  return (
    <Stack gap="xs">
      {disabled ? (
        <Alert color="yellow" icon={<IconInfoCircle size={16} />} role="note" py="xs">
          <Text size="sm">
            {tr(
              "ACLs are disabled here (Object Ownership: BucketOwnerEnforced), so the grants below cannot be changed. Change the bucket's Object Ownership to let ACLs work, or control access with the bucket policy.",
            )}
          </Text>
        </Alert>
      ) : null}
      <Table withTableBorder verticalSpacing={2} fz="xs" aria-label={tr("Grants")}>
        <Table.Thead>
          <Table.Tr>
            <Table.Th>{tr("Grantee")}</Table.Th>
            <Table.Th>{tr("Permission")}</Table.Th>
            <Table.Th w={40} />
          </Table.Tr>
        </Table.Thead>
        <Table.Tbody>
          {draft.map((grant) => (
            <Table.Tr key={grantId(grant)}>
              <Table.Td style={{ overflowWrap: "anywhere" }}>{granteeText(grant)}</Table.Td>
              <Table.Td>{grant.permission}</Table.Td>
              <Table.Td>
                <ActionIcon
                  variant="subtle"
                  color="red"
                  size="sm"
                  disabled={disabled}
                  aria-label={tr("Remove the grant {permission} for {who}", { permission: grant.permission, who: granteeText(grant) })}
                  onClick={() => setDraft((current) => current.filter((item) => grantId(item) !== grantId(grant)))}
                >
                  <IconTrash size={14} />
                </ActionIcon>
              </Table.Td>
            </Table.Tr>
          ))}
        </Table.Tbody>
      </Table>
      {disabled ? null : (
        <>
          <Group gap="xs" align="flex-start" wrap="wrap">
            <Select
              label={tr("Grantee type")}
              data={GRANTEE_KINDS.map((item) => ({ value: item.value, label: item.label }))}
              value={kind}
              onChange={(value) => {
                if (isKind(value)) {
                  setKind(value);
                  setTouched(false);
                }
              }}
              allowDeselect={false}
              comboboxProps={{ withinPortal: false }}
              w={170}
            />
            {kind === "group" ? (
              <Select
                label={tr("Group")}
                data={ACL_GROUPS.map((item) => ({ value: item.uri, label: item.label }))}
                value={group}
                onChange={(value) => {
                  if (value !== null) {
                    setGroup(value);
                  }
                }}
                allowDeselect={false}
                comboboxProps={{ withinPortal: false }}
                w={260}
              />
            ) : (
              <TextInput
                label={kind === "email" ? tr("E-mail address") : tr("AWS account ID")}
                description={
                  kind === "email"
                    ? tr("S3 accepts e-mail addresses in some regions only; the canonical account ID always works.")
                    : tr("The canonical user ID of the account, not the 12-digit account number.")
                }
                value={who}
                onChange={(event) => setWho(event.currentTarget.value)}
                error={touched ? granteeProblem : null}
                autoComplete="off"
                spellCheck={false}
                w={320}
              />
            )}
            <Select
              label={tr("Permission")}
              data={permissions.map((item) => ({ value: item.value, label: item.label }))}
              value={permission}
              onChange={(value) => {
                if (isPermission(value)) {
                  setPermission(value);
                }
              }}
              allowDeselect={false}
              comboboxProps={{ withinPortal: false }}
              w={300}
            />
            <Button variant="default" mt={25} leftSection={<IconPlus size={14} />} onClick={add}>
              {tr("Add grant")}
            </Button>
          </Group>
          {duplicate ? (
            <Text size="xs" c="dimmed">
              {tr("This grant is already in the list.")}
            </Text>
          ) : null}
        </>
      )}
      {addedPublic ? (
        <Alert color="orange" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
          <Text size="sm">{tr("These grants make the content reachable by people outside your account. The public access block may still stop them.")}</Text>
        </Alert>
      ) : null}
      {listProblem !== null ? (
        <Text size="sm" c="red" role="alert">
          {listProblem}
        </Text>
      ) : null}
      <ActionError message={action.error} />
      {disabled ? null : (
        <Group gap="xs">
          <Button loading={action.busy} disabled={!changed || listProblem !== null} onClick={() => void save()}>
            {tr("Save grants")}
          </Button>
          <Button variant="default" disabled={!changed || action.busy} onClick={() => setDraft(grants.map((grant) => ({ ...grant })))}>
            {tr("Reset")}
          </Button>
        </Group>
      )}
    </Stack>
  );
}

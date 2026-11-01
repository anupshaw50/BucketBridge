import { Badge, Button, Group, Stack, Table, Text, Tooltip } from "@mantine/core";
import { IconLock, IconPlus, IconShieldLock, IconStar } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { Account } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { Wizard } from "../../components/Wizard";
import {
  type AccountDraft,
  type DraftErrors,
  draftFromAccount,
  emptyDraft,
  isEmptyUpdate,
  providerLabel,
  toAccountInput,
  toAccountUpdate,
  validateConnection,
  validateCredentials,
} from "../../lib/accounts";
import { ContextMenu } from "../menus/ContextMenu";
import { accountMenu } from "../menus/surfaces";
import { isContextClick, useContextMenu } from "../menus/useContextMenu";
import { DiagnosticsPanel } from "../tools/DiagnosticsPanel";
import { makeDefault, removeAccount } from "./accountActions";
import { ConnectionFields, CredentialFields } from "./AccountFields";

function useDraft(initial: () => AccountDraft) {
  const [draft, setDraft] = useState(initial);
  const [errors, setErrors] = useState<DraftErrors>({});
  const update = (patch: Partial<AccountDraft>) => {
    setDraft((current) => ({ ...current, ...patch }));
    setErrors((current) => {
      const next = { ...current };
      for (const key of Object.keys(patch) as (keyof AccountDraft)[]) {
        delete next[key];
      }
      return next;
    });
  };
  return { draft, errors, setErrors, update };
}

function ReviewRow({ label, value }: { label: string; value: string }) {
  return (
    <Table.Tr>
      <Table.Th scope="row" w={180} fw={500}>
        {label}
      </Table.Th>
      <Table.Td style={{ overflowWrap: "anywhere" }}>{value}</Table.Td>
    </Table.Tr>
  );
}

const CONNECTION_FIELDS: ReadonlySet<string> = new Set(["label", "endpoint", "region"]);

/** Add-account wizard: connection, credentials, review. */
export function AddAccountDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const { draft, errors, setErrors, update } = useDraft(emptyDraft);
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const next = () => {
    const found = step === 0 ? validateConnection(draft, accounts, null) : validateCredentials(draft, null);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    setStep(step + 1);
    return true;
  };
  const finish = async () => {
    const found = { ...validateConnection(draft, accounts, null), ...validateCredentials(draft, null) };
    if (Object.keys(found).length > 0) {
      setErrors(found);
      setStep(Object.keys(found).some((key) => CONNECTION_FIELDS.has(key)) ? 0 : 1);
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const account = await model.api.add_account({ input: toAccountInput(draft) });
      await model.session.refreshAccounts();
      model.session.setActive(account.id);
      model.status.info(`Account "${account.label}" added.`);
      onClose();
    } catch (raw) {
      const failure = toBbError(raw);
      if (failure.code === "conflict") {
        setErrors({ label: failure.message });
        setStep(0);
      } else {
        setError(failure.message);
      }
    } finally {
      setBusy(false);
    }
  };

  const input = toAccountInput(draft);
  return (
    <AppModal opened onClose={onClose} title="Add account" size="lg" closeOnClickOutside={false}>
      <Wizard
        active={step}
        onNext={next}
        onBack={() => setStep(Math.max(0, step - 1))}
        onFinish={() => void finish()}
        onCancel={onClose}
        finishLabel="Add account"
        busy={busy}
        error={error}
        steps={[
          { label: "Connection", content: <ConnectionFields draft={draft} errors={errors} onChange={update} /> },
          {
            label: "Credentials",
            content: <CredentialFields draft={draft} errors={errors} onChange={update} original={null} />,
          },
          {
            label: "Review",
            content: (
              <Stack gap="xs">
                <Text size="sm">Check the details and add the account. The secret is stored in the credential store.</Text>
                <Table withTableBorder withRowBorders verticalSpacing={4} fz="sm">
                  <Table.Tbody>
                    <ReviewRow label="Label" value={input.label} />
                    <ReviewRow label="Endpoint" value={input.endpoint ?? "AWS"} />
                    <ReviewRow label="Region" value={input.region} />
                    <ReviewRow label="Path-style addressing" value={input.path_style ? "Yes" : "No"} />
                    <ReviewRow label="Requester pays" value={input.request_payer ? "Yes" : "No"} />
                    <ReviewRow label="Access key ID" value={input.access_key_id} />
                    <ReviewRow label="Secret access key" value="Entered (hidden)" />
                    <ReviewRow label="Session token" value={input.session_token === null ? "None" : "Entered (hidden)"} />
                    <ReviewRow label="MFA device" value={input.mfa_serial ?? "None"} />
                  </Table.Tbody>
                </Table>
                <DiagnosticsPanel target={{ kind: "input", input }} bucket={null} runLabel="Test connection" />
              </Stack>
            ),
          },
        ]}
      />
    </AppModal>
  );
}

/** Edits any field; an empty secret keeps the stored one. */
export function EditAccountDialog({
  accountId,
  onClose,
  focusMfa = false,
}: {
  accountId: string;
  onClose: () => void;
  focusMfa?: boolean;
}) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const account = accounts.find((item) => item.id === accountId) ?? null;
  if (account === null) {
    return (
      <OkCancelDialog opened title="Edit account" onOk={onClose} onCancel={onClose} okLabel="Close">
        <Text size="sm">This account no longer exists.</Text>
      </OkCancelDialog>
    );
  }
  return <EditAccountForm account={account} accounts={accounts} onClose={onClose} focusMfa={focusMfa} />;
}

function EditAccountForm({
  account,
  accounts,
  onClose,
  focusMfa,
}: {
  account: Account;
  accounts: Account[];
  onClose: () => void;
  focusMfa: boolean;
}) {
  const model = useModel();
  const { draft, errors, setErrors, update } = useDraft(() => draftFromAccount(account));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const save = async () => {
    const found = { ...validateConnection(draft, accounts, account.id), ...validateCredentials(draft, account) };
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return;
    }
    const change = toAccountUpdate(account, draft);
    if (isEmptyUpdate(change)) {
      onClose();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const updated = await model.api.update_account({ id: account.id, update: change });
      await model.session.refreshAccounts();
      model.status.info(`Account "${updated.label}" saved.`);
      if (model.session.store.getState().activeAccountId === account.id) {
        await model.browser.switchAccount(account.id);
      }
      onClose();
    } catch (raw) {
      const failure = toBbError(raw);
      if (failure.code === "conflict") {
        setErrors({ label: failure.message });
      } else {
        setError(failure.message);
      }
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={`Edit account ${account.label}`}
      okLabel="Save"
      onOk={() => void save()}
      onCancel={onClose}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm" fw={600}>
        Connection
      </Text>
      <ConnectionFields draft={draft} errors={errors} onChange={update} autoFocus={!focusMfa} />
      <Text size="sm" fw={600} mt="sm">
        Credentials
      </Text>
      <CredentialFields draft={draft} errors={errors} onChange={update} original={account} focusMfa={focusMfa} />
    </OkCancelDialog>
  );
}

/** Account list with add, edit, remove, default, switch and master-password actions. */
export function AccountManagerDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const activeId = useSlice(model.session.store, (state) => state.activeAccountId);
  const defaultId = useSlice(model.session.store, (state) => state.defaultAccountId);
  const security = useSlice(model.session.store, (state) => state.security);
  const masterSet = security?.master_password_set ?? false;
  const unlocked = security?.unlocked ?? true;
  const menu = useContextMenu();
  const openAccountMenu = (event: React.MouseEvent, account: Account) => {
    const built = accountMenu(model, account);
    menu.open(event, built.items, built.label);
  };
  const lockNow = async () => {
    try {
      await model.raw.lock();
      await model.session.refreshSecurity();
      model.status.info("Secrets locked.");
    } catch (error) {
      model.status.error(error, { notify: "Locking failed" });
    }
  };
  return (
    <AppModal opened onClose={onClose} title="Accounts" size={980}>
      <Stack gap="sm">
        <Group justify="space-between" wrap="nowrap">
          <Group gap="xs">
            <IconShieldLock size={16} aria-hidden />
            <Text size="sm">
              {masterSet
                ? `A master password protects the stored secrets. They are ${unlocked ? "unlocked" : "locked"}.`
                : "No master password is set. Secrets are protected by the operating system's credential store only."}
            </Text>
          </Group>
          <Group gap="xs" wrap="nowrap">
            {masterSet && unlocked ? (
              <Button size="xs" variant="default" leftSection={<IconLock size={14} />} onClick={() => void lockNow()}>
                Lock now
              </Button>
            ) : null}
            {masterSet && !unlocked ? (
              <Button size="xs" variant="default" onClick={() => model.prompts.showUnlock()}>
                Unlock
              </Button>
            ) : null}
            <Button size="xs" variant="default" onClick={() => model.dialogs.open({ kind: "master-password" })}>
              Master password...
            </Button>
          </Group>
        </Group>
        {accounts.length === 0 ? (
          <Text size="sm" c="dimmed" py="md">
            No accounts yet.
          </Text>
        ) : (
          <Table.ScrollContainer minWidth={860} maxHeight={360}>
            <Table striped highlightOnHover verticalSpacing={6} fz="sm" aria-label="Accounts">
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>Label</Table.Th>
                  <Table.Th>Service</Table.Th>
                  <Table.Th>Region</Table.Th>
                  <Table.Th>Access key ID</Table.Th>
                  <Table.Th>
                    <span className="bb-visually-hidden">Actions</span>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {accounts.map((account) => (
                  <Table.Tr
                    key={account.id}
                    onContextMenu={(event) => openAccountMenu(event, account)}
                    onClick={(event) => {
                      if (isContextClick(event)) {
                        openAccountMenu(event, account);
                      }
                    }}
                  >
                    <Table.Td>
                      <Group gap={6} wrap="nowrap">
                        <Text size="sm" fw={500} style={{ whiteSpace: "nowrap" }}>
                          {account.label}
                        </Text>
                        {account.id === defaultId ? (
                          <Badge size="xs" variant="light" leftSection={<IconStar size={10} />}>
                            Default
                          </Badge>
                        ) : null}
                        {account.id === activeId ? (
                          <Badge size="xs" variant="outline">
                            Active
                          </Badge>
                        ) : null}
                        {account.mfa_serial !== null ? (
                          <Tooltip label={`MFA device ${account.mfa_serial}`}>
                            <Badge size="xs" variant="light" color="grape">
                              MFA
                            </Badge>
                          </Tooltip>
                        ) : null}
                      </Group>
                    </Table.Td>
                    <Table.Td style={{ whiteSpace: "nowrap" }}>{providerLabel(account)}</Table.Td>
                    <Table.Td style={{ whiteSpace: "nowrap" }}>{account.region}</Table.Td>
                    <Table.Td ff="monospace" fz="xs">
                      {account.access_key_id}
                    </Table.Td>
                    <Table.Td>
                      <Group gap={4} justify="flex-end" wrap="nowrap">
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          disabled={account.id === activeId}
                          onClick={() => model.session.setActive(account.id)}
                          aria-label={`Use ${account.label}`}
                        >
                          Use
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          disabled={account.id === defaultId}
                          onClick={() => void makeDefault(model, account)}
                          aria-label={`Make ${account.label} the default`}
                        >
                          Set default
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          onClick={() => model.dialogs.open({ kind: "edit-account", accountId: account.id })}
                          aria-label={`Edit ${account.label}`}
                        >
                          Edit
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          color="red"
                          onClick={() => void removeAccount(model, account)}
                          aria-label={`Remove ${account.label}`}
                        >
                          Remove
                        </Button>
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                ))}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        )}
        <Group justify="space-between">
          <Button leftSection={<IconPlus size={14} />} onClick={() => model.dialogs.open({ kind: "add-account" })}>
            Add account...
          </Button>
          <Button variant="default" onClick={onClose}>
            Close
          </Button>
        </Group>
      </Stack>
      <ContextMenu menu={menu.state} onClose={menu.close} />
    </AppModal>
  );
}

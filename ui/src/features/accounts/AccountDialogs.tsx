import { ActionIcon, Badge, Button, Group, NativeSelect, Stack, Table, Text, Tooltip } from "@mantine/core";
import { IconArrowDown, IconArrowUp, IconFolder, IconLock, IconPlus, IconShieldLock, IconStar } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { Account, AccountLayout } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { Wizard } from "../../components/Wizard";
import {
  type AccountDraft,
  type DraftErrors,
  credentialsLabel,
  draftFromAccount,
  emptyDraft,
  isEmptyUpdate,
  providerLabel,
  toAccountInput,
  toAccountUpdate,
  validateConnection,
  validateCredentials,
} from "../../lib/accounts";
import {
  folderOf,
  groupAccounts,
  moveAccount,
  moveFolder,
  removeFolder,
  setAccountFolder,
} from "../../lib/accountGroups";
import { ContextMenu } from "../menus/ContextMenu";
import { accountMenu } from "../menus/surfaces";
import { isContextClick, useContextMenu } from "../menus/useContextMenu";
import { DiagnosticsPanel } from "../tools/DiagnosticsPanel";
import { makeDefault, removeAccount } from "./accountActions";
import { ConnectionFields, CredentialFields } from "./AccountFields";
import { InstanceFields, ProfileFields, RoleFields, SourcePicker, SsoFields } from "./SourceFields";

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

/** The credential fields of the kind of source the form describes. */
function SourceFields({
  draft,
  errors,
  onChange,
  accounts,
  original,
  focusMfa = false,
}: {
  draft: AccountDraft;
  errors: DraftErrors;
  onChange: (patch: Partial<AccountDraft>) => void;
  accounts: readonly Account[];
  original: Account | null;
  focusMfa?: boolean;
}) {
  switch (draft.source_kind) {
    case "keys":
      return <CredentialFields draft={draft} errors={errors} onChange={onChange} original={original} focusMfa={focusMfa} />;
    case "role":
      return <RoleFields draft={draft} errors={errors} onChange={onChange} accounts={accounts} editing={original !== null} />;
    case "sso":
      return <SsoFields draft={draft} errors={errors} onChange={onChange} editing={original !== null} />;
    case "instance":
      return <InstanceFields draft={draft} errors={errors} onChange={onChange} editing={original !== null} />;
    case "profile":
      return <ProfileFields draft={draft} />;
  }
}

/** The review rows of the add-account wizard for the chosen kind of source. */
function SourceReview({ draft, accounts }: { draft: AccountDraft; accounts: readonly Account[] }) {
  const entered = tr("Entered (hidden)");
  const none = tr("None");
  switch (draft.source_kind) {
    case "keys":
      return (
        <>
          <ReviewRow label={tr("Access key ID")} value={draft.access_key_id.trim()} />
          <ReviewRow label={tr("Secret access key")} value={entered} />
          <ReviewRow
            label={tr("Session token")}
            value={draft.session_token.trim() === "" ? none : entered}
          />
          <ReviewRow label={tr("MFA device")} value={draft.mfa_serial.trim() === "" ? none : draft.mfa_serial.trim()} />
        </>
      );
    case "role":
      return (
        <>
          <ReviewRow label={tr("Credentials")} value={tr("Assumed role")} />
          <ReviewRow
            label={tr("Signed with")}
            value={accounts.find((a) => a.id === draft.role_source_account_id)?.label ?? "-"}
          />
          <ReviewRow label={tr("Role ARN")} value={draft.role_arn.trim()} />
          <ReviewRow
            label={tr("Session")}
            value={tr("{name}, {n} minutes", { name: draft.role_session_name.trim(), n: draft.role_duration_minutes })}
          />
          <ReviewRow label={tr("External ID")} value={draft.role_external_id.trim() === "" ? none : draft.role_external_id.trim()} />
          <ReviewRow label={tr("MFA device")} value={draft.mfa_serial.trim() === "" ? none : draft.mfa_serial.trim()} />
        </>
      );
    case "sso":
      return (
        <>
          <ReviewRow label={tr("Credentials")} value={tr("AWS SSO (IAM Identity Center)")} />
          <ReviewRow label={tr("Start URL")} value={draft.sso_start_url.trim()} />
          <ReviewRow label={tr("AWS account")} value={draft.sso_account_id} />
          <ReviewRow label={tr("Role")} value={draft.sso_role_name} />
        </>
      );
    case "instance":
      return <ReviewRow label={tr("Credentials")} value={tr("Role of this EC2 instance or container")} />;
    case "profile":
      return <ReviewRow label={tr("Credentials")} value={tr("AWS profile {profile}", { profile: draft.profile })} />;
  }
}

/** Add-account wizard: connection, credentials, review. */
export function AddAccountDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const { draft, errors, setErrors, update } = useDraft(emptyDraft);
  const [step, setStep] = useState(0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const next = () => {
    const found = step === 0 ? validateConnection(draft, accounts, null) : validateCredentials(draft, null, accounts);
    setErrors(found);
    if (Object.keys(found).length > 0) {
      return false;
    }
    setStep(step + 1);
    return true;
  };
  const finish = async () => {
    const found = { ...validateConnection(draft, accounts, null), ...validateCredentials(draft, null, accounts) };
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
      model.status.info(tr("Account \"{label}\" added.", { label: account.label }));
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
    <AppModal opened onClose={onClose} title={tr("Add account")} size="lg" closeOnClickOutside={false}>
      <Wizard
        active={step}
        onNext={next}
        onBack={() => setStep(Math.max(0, step - 1))}
        onFinish={() => void finish()}
        onCancel={onClose}
        finishLabel={tr("Add account")}
        busy={busy}
        error={error}
        steps={[
          { label: tr("Connection"), content: <ConnectionFields draft={draft} errors={errors} onChange={update} /> },
          {
            label: tr("Credentials"),
            content: (
              <Stack gap="sm">
                <SourcePicker draft={draft} onChange={update} />
                <SourceFields draft={draft} errors={errors} onChange={update} accounts={accounts} original={null} />
              </Stack>
            ),
          },
          {
            label: tr("Review"),
            content: (
              <Stack gap="xs">
                <Text size="sm">
                  {draft.source_kind === "keys"
                    ? tr("Check the details and add the account. The secret is stored in the credential store.")
                    : tr("Check the details and add the account. No secret is stored: credentials are requested when needed.")}
                </Text>
                <Table withTableBorder withRowBorders verticalSpacing={4} fz="sm">
                  <Table.Tbody>
                    <ReviewRow label={tr("Label")} value={input.label} />
                    <ReviewRow label={tr("Endpoint")} value={input.endpoint ?? "AWS"} />
                    <ReviewRow label={tr("Region")} value={input.region} />
                    <ReviewRow label={tr("Path-style addressing")} value={input.path_style ? tr("Yes") : tr("No")} />
                    <ReviewRow label={tr("Requester pays")} value={input.request_payer ? tr("Yes") : tr("No")} />
                    <SourceReview draft={draft} accounts={accounts} />
                  </Table.Tbody>
                </Table>
                {draft.source_kind === "keys" ? (
                  <DiagnosticsPanel target={{ kind: "input", input }} bucket={null} runLabel={tr("Test connection")} />
                ) : null}
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
      <OkCancelDialog opened title={tr("Edit account")} onOk={onClose} onCancel={onClose} okLabel={tr("Close")}>
        <Text size="sm">{tr("This account no longer exists.")}</Text>
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
    const found = { ...validateConnection(draft, accounts, account.id), ...validateCredentials(draft, account, accounts) };
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
      model.status.info(tr("Account \"{label}\" saved.", { label: updated.label }));
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
      title={tr("Edit account {label}", { label: account.label })}
      okLabel={tr("Save")}
      onOk={() => void save()}
      onCancel={onClose}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm" fw={600}>
        {tr("Connection")}
      </Text>
      <ConnectionFields draft={draft} errors={errors} onChange={update} autoFocus={!focusMfa} />
      <Text size="sm" fw={600} mt="sm">
        {tr("Credentials")}
      </Text>
      <SourceFields
        draft={draft}
        errors={errors}
        onChange={update}
        accounts={accounts}
        original={account}
        focusMfa={focusMfa}
      />
    </OkCancelDialog>
  );
}

/** Account list with add, edit, remove, default, switch and master-password actions. */
export function AccountManagerDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accounts = useSlice(model.session.store, (state) => state.accounts);
  const folders = useSlice(model.session.store, (state) => state.folders);
  const activeId = useSlice(model.session.store, (state) => state.activeAccountId);
  const defaultId = useSlice(model.session.store, (state) => state.defaultAccountId);
  const security = useSlice(model.session.store, (state) => state.security);
  const groups = groupAccounts(accounts, folders);
  const applyLayout = async (layout: AccountLayout) => {
    try {
      await model.api.set_account_layout({ layout });
      await model.session.refreshAccounts();
    } catch (error) {
      model.status.error(error, { notify: tr("The accounts could not be arranged") });
    }
  };
  const exportAccounts = async () => {
    try {
      const path = await model.platform.pickSavePath({
        title: tr("Export accounts"),
        defaultPath: "bucketbridge-accounts.json",
        filters: [{ name: "JSON", extensions: ["json"] }],
      });
      if (path === null) {
        return;
      }
      const outcome = await model.api.export_accounts({ path, account_ids: null });
      model.status.info(
        tr("Exported {n} accounts to {path}. The file holds no secrets.", { n: outcome.accounts, path: outcome.path }),
      );
    } catch (error) {
      model.status.error(error, { notify: tr("The accounts could not be exported") });
    }
  };
  const removeFolderNow = async (name: string) => {
    const confirmed = await model.dialogs.confirm({
      title: tr("Remove folder"),
      message: tr("Remove the folder \"{name}\"? Its accounts are kept and are no longer in a folder.", { name }),
      confirmLabel: tr("Remove folder"),
    });
    if (confirmed) {
      await applyLayout(removeFolder(accounts, folders, name));
    }
  };
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
      model.status.info(tr("Secrets locked."));
    } catch (error) {
      model.status.error(error, { notify: tr("Locking failed") });
    }
  };
  return (
    <AppModal opened onClose={onClose} title={tr("Accounts")} size={980}>
      <Stack gap="sm">
        <Group justify="space-between" wrap="nowrap">
          <Group gap="xs">
            <IconShieldLock size={16} aria-hidden />
            <Text size="sm">
              {masterSet
                ? tr("A master password protects the stored secrets. They are {state}.", {
                    state: unlocked ? tr("unlocked") : tr("locked"),
                  })
                : tr("No master password is set. Secrets are protected by the operating system's credential store only.")}
            </Text>
          </Group>
          <Group gap="xs" wrap="nowrap">
            {masterSet && unlocked ? (
              <Button size="xs" variant="default" leftSection={<IconLock size={14} />} onClick={() => void lockNow()}>
                {tr("Lock now")}
              </Button>
            ) : null}
            {masterSet && !unlocked ? (
              <Button size="xs" variant="default" onClick={() => model.prompts.showUnlock()}>
                {tr("Unlock")}
              </Button>
            ) : null}
            <Button size="xs" variant="default" onClick={() => model.dialogs.open({ kind: "master-password" })}>
              {tr("Master password...")}
            </Button>
          </Group>
        </Group>
        {accounts.length === 0 ? (
          <Text size="sm" c="dimmed" py="md">
            {tr("No accounts yet.")}
          </Text>
        ) : (
          <Table.ScrollContainer minWidth={860} maxHeight={360}>
            <Table striped highlightOnHover verticalSpacing={6} fz="sm" aria-label={tr("Accounts")}>
              <Table.Thead>
                <Table.Tr>
                  <Table.Th>{tr("Label")}</Table.Th>
                  <Table.Th>{tr("Service")}</Table.Th>
                  <Table.Th>{tr("Region")}</Table.Th>
                  <Table.Th>{tr("Credentials")}</Table.Th>
                  <Table.Th>
                    <span className="bb-visually-hidden">{tr("Actions")}</span>
                  </Table.Th>
                </Table.Tr>
              </Table.Thead>
              <Table.Tbody>
                {groups.flatMap((group, groupIndex) => [
                  group.folder !== null ? (
                    <Table.Tr key={`folder:${group.folder}`} bg="var(--mantine-color-default-hover)">
                      <Table.Td colSpan={5}>
                        <Group gap="xs" wrap="nowrap" justify="space-between">
                          <Group gap={6} wrap="nowrap">
                            <IconFolder size={14} aria-hidden />
                            <Text size="sm" fw={600}>
                              {group.folder}
                            </Text>
                            <Text size="xs" c="dimmed">
                              {group.accounts.length === 1
                                ? tr("{n} account", { n: group.accounts.length })
                                : tr("{n} accounts", { n: group.accounts.length })}
                            </Text>
                          </Group>
                          <Group gap={2} wrap="nowrap">
                            <ActionIcon
                              size="sm"
                              variant="subtle"
                              disabled={groupIndex === 0}
                              aria-label={tr("Move folder {name} up", { name: group.folder })}
                              onClick={() => void applyLayout(moveFolder(accounts, folders, group.folder ?? "", "up"))}
                            >
                              <IconArrowUp size={14} />
                            </ActionIcon>
                            <ActionIcon
                              size="sm"
                              variant="subtle"
                              disabled={groupIndex >= folders.length - 1}
                              aria-label={tr("Move folder {name} down", { name: group.folder })}
                              onClick={() => void applyLayout(moveFolder(accounts, folders, group.folder ?? "", "down"))}
                            >
                              <IconArrowDown size={14} />
                            </ActionIcon>
                            <Button
                              size="compact-xs"
                              variant="subtle"
                              onClick={() => model.dialogs.open({ kind: "account-folder", folder: group.folder ?? "" })}
                              aria-label={tr("Rename folder {name}", { name: group.folder })}
                            >
                              {tr("Rename")}
                            </Button>
                            <Button
                              size="compact-xs"
                              variant="subtle"
                              color="red"
                              onClick={() => void removeFolderNow(group.folder ?? "")}
                              aria-label={tr("Remove folder {name}", { name: group.folder })}
                            >
                              {tr("Remove")}
                            </Button>
                          </Group>
                        </Group>
                      </Table.Td>
                    </Table.Tr>
                  ) : folders.length > 0 ? (
                    <Table.Tr key="folder:none" bg="var(--mantine-color-default-hover)">
                      <Table.Td colSpan={5}>
                        <Text size="sm" fw={600} c="dimmed">
                          {tr("Not in a folder")}
                        </Text>
                      </Table.Td>
                    </Table.Tr>
                  ) : null,
                  ...group.accounts.map((account, index) => (
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
                            {tr("Default")}
                          </Badge>
                        ) : null}
                        {account.id === activeId ? (
                          <Badge size="xs" variant="outline">
                            {tr("Active")}
                          </Badge>
                        ) : null}
                        {account.source.type === "static_keys" && account.secret_ref === "" ? (
                          <Tooltip label={tr("Imported without a secret: edit the account and enter its secret access key.")}>
                            <Badge size="xs" variant="light" color="orange">
                              {tr("Needs secret")}
                            </Badge>
                          </Tooltip>
                        ) : null}
                        {account.mfa_serial !== null ? (
                          <Tooltip label={tr("MFA device {serial}", { serial: account.mfa_serial })}>
                            <Badge size="xs" variant="light" color="grape">
                              MFA
                            </Badge>
                          </Tooltip>
                        ) : null}
                      </Group>
                    </Table.Td>
                    <Table.Td style={{ whiteSpace: "nowrap" }}>{providerLabel(account)}</Table.Td>
                    <Table.Td style={{ whiteSpace: "nowrap" }}>{account.region}</Table.Td>
                    <Table.Td ff={account.source.type === "static_keys" ? "monospace" : undefined} fz="xs">
                      {credentialsLabel(account, accounts)}
                    </Table.Td>
                    <Table.Td>
                      <Group gap={4} justify="flex-end" wrap="nowrap">
                        <NativeSelect
                          size="xs"
                          aria-label={tr("Folder of {label}", { label: account.label })}
                          value={folderOf(folders, account.id) ?? ""}
                          data={[{ value: "", label: tr("No folder") }, ...folders.map((folder) => ({ value: folder.name, label: folder.name }))]}
                          onChange={(event) =>
                            void applyLayout(setAccountFolder(accounts, folders, account.id, event.currentTarget.value || null))
                          }
                        />
                        <ActionIcon
                          size="sm"
                          variant="subtle"
                          disabled={index === 0}
                          aria-label={tr("Move {label} up", { label: account.label })}
                          onClick={() => void applyLayout(moveAccount(accounts, folders, account.id, "up"))}
                        >
                          <IconArrowUp size={14} />
                        </ActionIcon>
                        <ActionIcon
                          size="sm"
                          variant="subtle"
                          disabled={index === group.accounts.length - 1}
                          aria-label={tr("Move {label} down", { label: account.label })}
                          onClick={() => void applyLayout(moveAccount(accounts, folders, account.id, "down"))}
                        >
                          <IconArrowDown size={14} />
                        </ActionIcon>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          disabled={account.id === activeId}
                          onClick={() => model.session.setActive(account.id)}
                          aria-label={tr("Use {label}", { label: account.label })}
                        >
                          {tr("Use")}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          disabled={account.id === defaultId}
                          onClick={() => void makeDefault(model, account)}
                          aria-label={tr("Make {label} the default", { label: account.label })}
                        >
                          {tr("Set default")}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          onClick={() => model.dialogs.open({ kind: "edit-account", accountId: account.id })}
                          aria-label={tr("Edit {label}", { label: account.label })}
                        >
                          {tr("Edit")}
                        </Button>
                        <Button
                          size="compact-xs"
                          variant="subtle"
                          color="red"
                          onClick={() => void removeAccount(model, account)}
                          aria-label={tr("Remove {label}", { label: account.label })}
                        >
                          {tr("Remove")}
                        </Button>
                      </Group>
                    </Table.Td>
                  </Table.Tr>
                  )),
                ])}
              </Table.Tbody>
            </Table>
          </Table.ScrollContainer>
        )}
        <Group justify="space-between">
          <Group gap="xs">
            <Button leftSection={<IconPlus size={14} />} onClick={() => model.dialogs.open({ kind: "add-account" })}>
              {tr("Add account...")}
            </Button>
            <Button variant="default" onClick={() => model.dialogs.open({ kind: "import-aws" })}>
              {tr("Import from AWS config...")}
            </Button>
            <Button variant="default" onClick={() => model.dialogs.open({ kind: "account-folder" })}>
              {tr("New folder...")}
            </Button>
            <Button variant="default" disabled={accounts.length === 0} onClick={() => void exportAccounts()}>
              {tr("Export accounts...")}
            </Button>
            <Button variant="default" onClick={() => model.dialogs.open({ kind: "import-accounts" })}>
              {tr("Import accounts...")}
            </Button>
          </Group>
          <Button variant="default" onClick={onClose}>
            {tr("Close")}
          </Button>
        </Group>
      </Stack>
      <ContextMenu menu={menu.state} onClose={menu.close} />
    </AppModal>
  );
}

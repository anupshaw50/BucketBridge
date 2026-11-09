import { Alert, Badge, Button, Checkbox, Code, Group, PasswordInput, ScrollArea, Select, Stack, Table, Text, Textarea, TextInput } from "@mantine/core";
import { useCallback, useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import type { IamManagedPolicy, IamNewAccessKey, IamSection, IamStatus, IamUser, IamUserDetails } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { AppModal } from "../../components/AppModal";

/** The value of a section that loaded, else an empty fallback (the failure is shown next to it). */
function loaded<T>(section: IamSection<T>, fallback: T): T {
  return section.type === "loaded" ? section.value : fallback;
}

function failure<T>(section: IamSection<T>): string | null {
  return section.type === "failed" ? section.error.message : section.type === "unsupported" ? section.message : null;
}

/** The one-time display of a new access key: the secret is held in this component only and dropped on dismiss. */
function SecretPanel({ created, onDismiss }: { created: IamNewAccessKey; onDismiss: () => void }) {
  const model = useModel();
  const [copied, setCopied] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const copy = async () => {
    try {
      await model.platform.copyText(created.secret_access_key, { sensitive: true });
      setCopied(true);
    } catch (raw) {
      setProblem(toBbError(raw).message);
    }
  };
  return (
    <Alert color="yellow" title={tr("Copy the secret access key now")} data-testid="iam-secret">
      <Stack gap="xs">
        <Text size="sm">
          {tr(
            "This is the only time the secret access key is shown. It is not stored anywhere and cannot be recovered; if you lose it, create a new key.",
          )}
        </Text>
        <Text size="sm">
          {tr("Access key ID")}: <Code>{created.access_key_id}</Code>
        </Text>
        <Text size="sm">
          {tr("Secret access key")}: <Code data-testid="iam-secret-value">{created.secret_access_key}</Code>
        </Text>
        {problem !== null ? <Text c="red" size="sm">{problem}</Text> : null}
        <Group>
          <Button size="xs" onClick={() => void copy()}>
            {copied ? tr("Copied") : tr("Copy secret")}
          </Button>
          <Button size="xs" variant="default" onClick={onDismiss}>
            {tr("I have saved it")}
          </Button>
        </Group>
      </Stack>
    </Alert>
  );
}

function UserPanel({ accountId, user, onChanged, onDeleted }: { accountId: string; user: string; onChanged: () => void; onDeleted: () => void }) {
  const model = useModel();
  const [details, setDetails] = useState<IamUserDetails | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [secret, setSecret] = useState<IamNewAccessKey | null>(null);
  const [catalog, setCatalog] = useState<IamManagedPolicy[]>([]);
  const [arn, setArn] = useState<string | null>(null);
  const [policyName, setPolicyName] = useState("");
  const [document, setDocument] = useState("");
  const [password, setPassword] = useState("");
  const [reset, setReset] = useState(true);

  const reload = useCallback(async () => {
    try {
      setDetails(await model.api.iam_user_details({ account_id: accountId, name: user }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  }, [model, accountId, user]);

  useEffect(() => {
    void (async () => {
      await Promise.resolve();
      await reload();
    })();
    void model.api.iam_list_managed_policies({ account_id: accountId, scope: "all" }).then(setCatalog, () => setCatalog([]));
  }, [model, accountId, user, reload]);

  const act = async (work: () => Promise<void>) => {
    setError(null);
    try {
      await work();
      await reload();
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const confirmTyped = (title: string, message: string, text: string, label: string) =>
    model.dialogs.confirm({ title, message, confirmLabel: label, danger: true, typedConfirmation: text });

  const deleteUser = async () => {
    if (
      !(await confirmTyped(
        tr("Delete user"),
        tr("Delete {user} with its access keys, policies, login profile and group memberships? This cannot be undone.", { user }),
        user,
        tr("Delete user"),
      ))
    ) {
      return;
    }
    setError(null);
    try {
      const outcome = await model.api.iam_delete_user({ account_id: accountId, name: user, confirm: user });
      if (outcome.deleted) {
        model.status.info(tr("User {user} deleted.", { user }));
        onDeleted();
      } else {
        const failed = outcome.steps
          .filter((step) => !step.ok)
          .map((step) => `${step.kind} ${step.target}: ${step.error?.message ?? tr("failed")}`);
        setError(`${tr("The user was not deleted.")} ${failed.join("; ")}`);
        await reload();
      }
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  const keys = details === null ? [] : loaded(details.access_keys, []);
  const managed = details === null ? [] : loaded(details.managed_policies, []);
  const inline = details === null ? [] : loaded(details.inline_policies, []);
  const profile = details === null ? null : loaded(details.login_profile, null);

  return (
    <Stack gap="sm" data-testid="iam-user-panel">
      <Group justify="space-between">
        <Text fw={600}>{user}</Text>
        <Button size="xs" color="red" variant="light" onClick={() => void deleteUser()}>
          {tr("Delete user...")}
        </Button>
      </Group>
      {error !== null ? <Alert color="red">{error}</Alert> : null}
      {secret !== null ? <SecretPanel created={secret} onDismiss={() => setSecret(null)} /> : null}

      <Text fw={600} size="sm">{tr("Managed policies")}</Text>
      {details !== null && failure(details.managed_policies) !== null ? <Text size="xs" c="red">{failure(details.managed_policies)}</Text> : null}
      {managed.map((policy) => (
        <Group key={policy.arn} justify="space-between">
          <Text size="sm" style={{ overflowWrap: "anywhere" }}>{policy.name}</Text>
          <Button size="compact-xs" variant="default" onClick={() => void act(() => model.api.iam_detach_policy({ account_id: accountId, user, policy_arn: policy.arn }))}>
            {tr("Detach")}
          </Button>
        </Group>
      ))}
      <Group align="flex-end" wrap="nowrap">
        <Select
          label={tr("Attach a policy")}
          searchable
          placeholder={tr("Choose a managed policy")}
          data={catalog.filter((p) => !managed.some((m) => m.arn === p.arn)).map((p) => ({ value: p.arn, label: p.name }))}
          value={arn}
          onChange={setArn}
          style={{ flex: 1 }}
        />
        <Button
          size="xs"
          disabled={arn === null}
          onClick={() => {
            if (arn !== null) {
              void act(() => model.api.iam_attach_policy({ account_id: accountId, user, policy_arn: arn })).then(() => setArn(null));
            }
          }}
        >
          {tr("Attach")}
        </Button>
      </Group>

      <Text fw={600} size="sm">{tr("Inline policies")}</Text>
      {inline.map((name) => (
        <Group key={name} justify="space-between">
          <Text size="sm">{name}</Text>
          <Group gap={4}>
            <Button
              size="compact-xs"
              variant="default"
              onClick={() =>
                void act(async () => {
                  const policy = await model.api.iam_get_inline_policy({ account_id: accountId, user, name });
                  setPolicyName(policy.name);
                  setDocument(policy.document);
                })
              }
            >
              {tr("Edit")}
            </Button>
            <Button
              size="compact-xs"
              color="red"
              variant="light"
              onClick={() =>
                void (async () => {
                  if (await confirmTyped(tr("Delete inline policy"), tr("Delete the inline policy {name} of {user}?", { name, user }), name, tr("Delete policy"))) {
                    await act(() => model.api.iam_delete_inline_policy({ account_id: accountId, user, name, confirm: name }));
                  }
                })()
              }
            >
              {tr("Delete...")}
            </Button>
          </Group>
        </Group>
      ))}
      <TextInput label={tr("Policy name")} value={policyName} onChange={(e) => setPolicyName(e.currentTarget.value)} />
      <Textarea label={tr("Policy document (JSON)")} rows={6} value={document} onChange={(e) => setDocument(e.currentTarget.value)} styles={{ input: { fontFamily: "monospace" } }} />
      <Group>
        <Button size="xs" disabled={policyName === "" || document === ""} onClick={() => void act(async () => {
          await model.api.iam_put_inline_policy({ account_id: accountId, user, name: policyName, document });
          setPolicyName("");
          setDocument("");
        })}>
          {tr("Save inline policy")}
        </Button>
      </Group>

      <Text fw={600} size="sm">{tr("Access keys")}</Text>
      {details !== null && failure(details.access_keys) !== null ? <Text size="xs" c="red">{failure(details.access_keys)}</Text> : null}
      {keys.length > 0 ? (
        <Table>
          <Table.Tbody>
            {keys.map((key) => (
              <Table.Tr key={key.access_key_id}>
                <Table.Td><Code>{key.access_key_id}</Code></Table.Td>
                <Table.Td><Badge color={key.status === "active" ? "green" : "gray"}>{key.status}</Badge></Table.Td>
                <Table.Td>
                  <Group gap={4} justify="flex-end">
                    <Button size="compact-xs" variant="default" onClick={() => void act(() => model.api.iam_set_access_key_active({ account_id: accountId, user, access_key_id: key.access_key_id, active: key.status !== "active" }))}>
                      {key.status === "active" ? tr("Deactivate") : tr("Activate")}
                    </Button>
                    <Button
                      size="compact-xs"
                      color="red"
                      variant="light"
                      onClick={() =>
                        void (async () => {
                          if (await confirmTyped(tr("Delete access key"), tr("Delete the access key {key}? Anything that uses it stops working.", { key: key.access_key_id }), key.access_key_id, tr("Delete key"))) {
                            await act(() => model.api.iam_delete_access_key({ account_id: accountId, user, access_key_id: key.access_key_id, confirm: key.access_key_id }));
                          }
                        })()
                      }
                    >
                      {tr("Delete...")}
                    </Button>
                  </Group>
                </Table.Td>
              </Table.Tr>
            ))}
          </Table.Tbody>
        </Table>
      ) : (
        <Text size="sm" c="dimmed">{tr("No access keys.")}</Text>
      )}
      <Group>
        <Button size="xs" onClick={() => void act(async () => setSecret(await model.api.iam_create_access_key({ account_id: accountId, user })))}>
          {tr("Create access key")}
        </Button>
      </Group>

      <Text fw={600} size="sm">{tr("Console login profile")}</Text>
      {profile !== null ? (
        <Group justify="space-between">
          <Text size="sm">{profile.password_reset_required ? tr("Password set (reset required at next sign-in)") : tr("Password set")}</Text>
          <Button
            size="compact-xs"
            color="red"
            variant="light"
            onClick={() =>
              void (async () => {
                if (await confirmTyped(tr("Delete login profile"), tr("Remove the console password of {user}?", { user }), user, tr("Delete profile"))) {
                  await act(() => model.api.iam_delete_login_profile({ account_id: accountId, user, confirm: user }));
                }
              })()
            }
          >
            {tr("Delete...")}
          </Button>
        </Group>
      ) : (
        <Stack gap="xs">
          <PasswordInput label={tr("Console password")} value={password} onChange={(e) => setPassword(e.currentTarget.value)} autoComplete="new-password" />
          <Checkbox label={tr("Require a new password at the next sign-in")} checked={reset} onChange={(e) => setReset(e.currentTarget.checked)} />
          <Group>
            <Button size="xs" disabled={password === ""} onClick={() => void act(async () => {
              await model.api.iam_create_login_profile({ account_id: accountId, user, password, reset_required: reset });
              setPassword("");
            })}>
              {tr("Create login profile")}
            </Button>
          </Group>
        </Stack>
      )}
      <Button size="xs" variant="subtle" onClick={onChanged}>
        {tr("Refresh list")}
      </Button>
    </Stack>
  );
}

/** The IAM manager: the users of the active account, with their policies, access keys and login profile. */
export function IamDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const [status, setStatus] = useState<IamStatus | null>(null);
  const [users, setUsers] = useState<IamUser[] | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (accountId === null) {
      return;
    }
    setError(null);
    try {
      setUsers(await model.api.iam_list_users({ account_id: accountId }));
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  }, [model, accountId]);

  useEffect(() => {
    if (accountId === null) {
      return;
    }
    void (async () => {
      try {
        const next = await model.api.iam_status({ account_id: accountId });
        setStatus(next);
        if (next.supported) {
          await refresh();
        }
      } catch (raw) {
        setError(toBbError(raw).message);
      }
    })();
  }, [model, accountId, refresh]);

  const create = async () => {
    if (accountId === null) {
      return;
    }
    try {
      const user = await model.api.iam_create_user({ account_id: accountId, name, path: null });
      setName("");
      setSelected(user.name);
      await refresh();
    } catch (raw) {
      setError(toBbError(raw).message);
    }
  };

  let body;
  if (accountId === null) {
    body = <Text size="sm">{tr("Add or choose an account first.")}</Text>;
  } else if (status !== null && !status.supported) {
    body = <Alert color="gray">{status.message ?? tr("IAM is not available for this endpoint")}</Alert>;
  } else {
    body = (
      <Group align="flex-start" wrap="nowrap" gap="md">
        <Stack gap="xs" w={240}>
          <Group align="flex-end" wrap="nowrap" gap="xs">
            <TextInput label={tr("New user")} placeholder={tr("user name")} value={name} onChange={(e) => setName(e.currentTarget.value)} style={{ flex: 1 }} />
            <Button size="xs" disabled={name === ""} onClick={() => void create()}>
              {tr("Create")}
            </Button>
          </Group>
          <ScrollArea h={380}>
            {(users ?? []).map((user) => (
              <Button key={user.name} fullWidth justify="flex-start" variant={selected === user.name ? "light" : "subtle"} onClick={() => setSelected(user.name)}>
                {user.name}
              </Button>
            ))}
            {users !== null && users.length === 0 ? <Text size="sm" c="dimmed">{tr("No users.")}</Text> : null}
          </ScrollArea>
        </Stack>
        <ScrollArea h={440} style={{ flex: 1 }}>
          {error !== null ? <Alert color="red" mb="xs">{error}</Alert> : null}
          {selected !== null ? (
            <UserPanel
              key={selected}
              accountId={accountId}
              user={selected}
              onChanged={() => void refresh()}
              onDeleted={() => {
                setSelected(null);
                void refresh();
              }}
            />
          ) : (
            <Text size="sm" c="dimmed">{tr("Choose a user.")}</Text>
          )}
        </ScrollArea>
      </Group>
    );
  }
  return (
    <AppModal opened onClose={onClose} title={tr("IAM manager")} size="xl" closeOnClickOutside={false}>
      <Stack gap="sm">
        {body}
        <Group justify="flex-end">
          <Button variant="default" onClick={onClose}>
            {tr("Close")}
          </Button>
        </Group>
      </Stack>
    </AppModal>
  );
}

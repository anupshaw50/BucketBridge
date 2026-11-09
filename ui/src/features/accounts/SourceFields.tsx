import { Alert, Anchor, Button, Code, Group, NumberInput, SegmentedControl, Select, Stack, Text, TextInput } from "@mantine/core";
import { useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { Account, SsoAccount, SsoLoginStarted } from "../../api/types";
import { useModel } from "../../app/context";
import { tr } from "../../i18n";
import {
  type AccountDraft,
  type DraftErrors,
  MAX_ROLE_MINUTES,
  MIN_ROLE_MINUTES,
  type SourceKind,
} from "../../lib/accounts";

interface SourceProps {
  draft: AccountDraft;
  errors: DraftErrors;
  onChange: (patch: Partial<AccountDraft>) => void;
}

const KIND_LABELS: { value: SourceKind; label: string }[] = [
  { value: "keys", label: tr("Access keys") },
  { value: "role", label: tr("Assume a role") },
  { value: "sso", label: "AWS SSO" },
  { value: "instance", label: tr("Instance role") },
];

/** Chooses where the credentials of a new account come from. */
export function SourcePicker({ draft, onChange }: Pick<SourceProps, "draft" | "onChange">) {
  return (
    <Stack gap={4}>
      <Text size="sm" fw={500}>
        {tr("Credentials from")}
      </Text>
      <SegmentedControl<SourceKind>
        size="xs"
        fullWidth
        value={draft.source_kind}
        onChange={(value) => onChange({ source_kind: value })}
        data={KIND_LABELS}
        aria-label={tr("Credentials from")}
      />
      <Text size="xs" c="dimmed">
        {SOURCE_HELP[draft.source_kind]}
      </Text>
    </Stack>
  );
}

const SOURCE_HELP: Record<SourceKind, string> = {
  keys: tr("An access key and secret key, kept in the operating system's credential store."),
  role: tr("Temporary credentials for an IAM role, requested with another account of yours. Nothing secret is stored."),
  sso: tr("Sign in to IAM Identity Center (AWS SSO) in your browser and choose an account and role. Nothing secret is stored."),
  instance: tr("The role of the EC2 instance or ECS or EKS container BucketBridge runs on. Nothing secret is stored."),
  profile: tr("A profile of your AWS config files. The files stay the source of the credentials."),
};

/** Assumed role: source account, role, session settings and optional MFA device. */
export function RoleFields({
  draft,
  errors,
  onChange,
  accounts,
  editing,
}: SourceProps & { accounts: readonly Account[]; editing: boolean }) {
  const sources = accounts.map((account) => ({ value: account.id, label: account.label }));
  return (
    <Stack gap="sm">
      <Select
        label={tr("Sign the request with")}
        description={tr("The account whose credentials call STS AssumeRole. It can be any account, including another role.")}
        data={sources}
        value={draft.role_source_account_id === "" ? null : draft.role_source_account_id}
        onChange={(value) => onChange({ role_source_account_id: value ?? "" })}
        error={errors.role_source_account_id}
        disabled={editing}
        allowDeselect={false}
        comboboxProps={{ withinPortal: false }}
        placeholder={tr("Choose an account")}
        required
        data-autofocus={editing ? undefined : true}
      />
      <TextInput
        label={tr("Role ARN")}
        placeholder="arn:aws:iam::123456789012:role/Name"
        value={draft.role_arn}
        onChange={(event) => onChange({ role_arn: event.currentTarget.value })}
        error={errors.role_arn}
        spellCheck={false}
        autoComplete="off"
        required
      />
      <Group grow align="flex-start">
        <TextInput
          label={tr("Session name")}
          value={draft.role_session_name}
          onChange={(event) => onChange({ role_session_name: event.currentTarget.value })}
          error={errors.role_session_name}
          spellCheck={false}
        />
        <NumberInput
          label={tr("Session length (minutes)")}
          description={`${MIN_ROLE_MINUTES} to ${MAX_ROLE_MINUTES}. ${tr("Credentials refresh before they expire.")}`}
          value={draft.role_duration_minutes === "" ? "" : Number(draft.role_duration_minutes)}
          onChange={(value) => onChange({ role_duration_minutes: String(value) })}
          error={errors.role_duration_minutes}
          min={MIN_ROLE_MINUTES}
          max={MAX_ROLE_MINUTES}
          allowDecimal={false}
          clampBehavior="none"
        />
      </Group>
      <TextInput
        label={tr("External ID (optional)")}
        description={tr("Only if the role's trust policy asks for one.")}
        value={draft.role_external_id}
        onChange={(event) => onChange({ role_external_id: event.currentTarget.value })}
        error={errors.role_external_id}
        spellCheck={false}
        autoComplete="off"
      />
      <TextInput
        label={tr("MFA device (optional)")}
        description={tr("ARN or serial number of the device the role's trust policy asks for. You enter a code each time the session ends.")}
        value={draft.mfa_serial}
        onChange={(event) => onChange({ mfa_serial: event.currentTarget.value })}
        error={errors.mfa_serial}
        spellCheck={false}
        autoComplete="off"
      />
    </Stack>
  );
}

/** Instance role: check that this machine has one. */
export function InstanceFields({ draft, errors, onChange, editing }: SourceProps & { editing: boolean }) {
  const model = useModel();
  const [busy, setBusy] = useState(false);
  const [found, setFound] = useState<string | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const check = async () => {
    setBusy(true);
    setFailure(null);
    try {
      const probe = await model.api.probe_instance_role();
      setFound(
        tr("Found the role of this {machine} (access key {accessKeyId}).", {
          machine: probe.kind === "container" ? tr("container") : tr("instance"),
          accessKeyId: probe.access_key_id,
        }),
      );
      onChange({ instance_checked: true });
    } catch (raw) {
      setFound(null);
      setFailure(toBbError(raw).message);
      onChange({ instance_checked: false });
    } finally {
      setBusy(false);
    }
  };
  return (
    <Stack gap="sm">
      <Text size="sm">
        {tr(
          "BucketBridge asks the EC2 instance metadata service (version 2) or the container credentials endpoint for the role of this machine, and refreshes the credentials before they expire. This only works on an EC2 instance or an ECS or EKS container.",
        )}
      </Text>
      {editing ? null : (
        <Group>
          <Button variant="default" loading={busy} onClick={() => void check()}>
            {tr("Check this machine")}
          </Button>
        </Group>
      )}
      {found !== null ? (
        <Alert color="green" variant="light" role="status">
          {found}
        </Alert>
      ) : null}
      {failure !== null ? (
        <Alert color="red" variant="light" role="alert">
          {failure}
        </Alert>
      ) : null}
      {errors.instance_checked !== undefined && draft.instance_checked === false && failure === null ? (
        <Text size="sm" c="red" role="alert">
          {errors.instance_checked}
        </Text>
      ) : null}
    </Stack>
  );
}

/** A linked AWS profile: read-only. */
export function ProfileFields({ draft }: Pick<SourceProps, "draft">) {
  return (
    <Stack gap="xs">
      <Text size="sm">
        {tr(
          "This account uses the profile {profile} of your AWS config files. Change keys, roles or SSO settings in those files; BucketBridge reads them whenever it needs credentials.",
          { profile: draft.profile },
        )}
      </Text>
    </Stack>
  );
}

/** The sign-in state of the SSO flow. */
type Phase = "idle" | "starting" | "waiting" | "signed-in";

/** IAM Identity Center: portal, browser sign-in and the choice of account and role. */
export function SsoFields({ draft, errors, onChange, editing }: SourceProps & { editing: boolean }) {
  const model = useModel();
  const [phase, setPhase] = useState<Phase>("idle");
  const [started, setStarted] = useState<SsoLoginStarted | null>(null);
  const [failure, setFailure] = useState<string | null>(null);
  const [accounts, setAccounts] = useState<SsoAccount[]>([]);
  const [roles, setRoles] = useState<string[]>([]);
  const login = useRef<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const alive = useRef(true);

  const portal = {
    start_url: draft.sso_start_url.trim(),
    sso_region: draft.sso_region.trim(),
    sso_session: draft.sso_session.trim() === "" ? null : draft.sso_session.trim(),
  };

  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
      if (timer.current !== null) {
        clearTimeout(timer.current);
      }
      if (login.current !== null) {
        void model.api.sso_cancel_login({ login_id: login.current }).catch(() => undefined);
      }
    };
  }, [model]);

  const loadAccounts = async () => {
    try {
      const listed = await model.api.sso_list_accounts(portal);
      if (alive.current) {
        setAccounts(listed);
      }
    } catch (raw) {
      if (alive.current) {
        setFailure(toBbError(raw).message);
      }
    }
  };

  const loadRoles = async (accountId: string) => {
    onChange({ sso_account_id: accountId, sso_role_name: "" });
    setRoles([]);
    try {
      const listed = await model.api.sso_list_roles({ ...portal, account_id: accountId });
      if (alive.current) {
        setRoles(listed);
        if (listed.length === 1 && listed[0] !== undefined) {
          onChange({ sso_account_id: accountId, sso_role_name: listed[0] });
        }
      }
    } catch (raw) {
      if (alive.current) {
        setFailure(toBbError(raw).message);
      }
    }
  };

  const poll = (id: string, seconds: number) => {
    timer.current = setTimeout(
      () => {
        void (async () => {
          try {
            const result = await model.api.sso_poll_login({ login_id: id });
            if (!alive.current) {
              return;
            }
            if (result.state === "pending") {
              poll(id, result.interval_secs);
              return;
            }
            login.current = null;
            setPhase("signed-in");
            await loadAccounts();
          } catch (raw) {
            if (alive.current) {
              login.current = null;
              setPhase("idle");
              setFailure(toBbError(raw).message);
            }
          }
        })();
      },
      Math.max(1, seconds) * 1000,
    );
  };

  const signIn = async () => {
    setFailure(null);
    if (!portal.start_url.startsWith("https://") || portal.sso_region === "") {
      setFailure(tr("Enter the portal start URL (https://...) and the region first."));
      return;
    }
    setPhase("starting");
    try {
      const result = await model.api.sso_start_login(portal);
      if (!alive.current) {
        return;
      }
      login.current = result.login_id;
      setStarted(result);
      setPhase("waiting");
      poll(result.login_id, result.interval_secs);
    } catch (raw) {
      if (alive.current) {
        setPhase("idle");
        setFailure(toBbError(raw).message);
      }
    }
  };

  const cancel = () => {
    if (timer.current !== null) {
      clearTimeout(timer.current);
    }
    if (login.current !== null) {
      void model.api.sso_cancel_login({ login_id: login.current }).catch(() => undefined);
      login.current = null;
    }
    setPhase("idle");
  };

  const target = started?.verification_uri_complete ?? started?.verification_uri ?? "";
  return (
    <Stack gap="sm">
      <TextInput
        label={tr("Start URL")}
        description={tr("The AWS access portal URL of your organization.")}
        placeholder="https://example.awsapps.com/start"
        value={draft.sso_start_url}
        onChange={(event) => onChange({ sso_start_url: event.currentTarget.value })}
        error={errors.sso_start_url}
        spellCheck={false}
        autoComplete="off"
        disabled={editing}
        required
        data-autofocus={editing ? undefined : true}
      />
      <Group grow align="flex-start">
        <TextInput
          label={tr("SSO region")}
          description={tr("The region IAM Identity Center runs in.")}
          placeholder="eu-west-1"
          value={draft.sso_region}
          onChange={(event) => onChange({ sso_region: event.currentTarget.value })}
          error={errors.sso_region}
          spellCheck={false}
          disabled={editing}
          required
        />
        <TextInput
          label={tr("Session name (optional)")}
          description={tr("The sso-session of your AWS config, if you use one.")}
          value={draft.sso_session}
          onChange={(event) => onChange({ sso_session: event.currentTarget.value })}
          spellCheck={false}
          disabled={editing}
        />
      </Group>
      <Group>
        <Button
          variant="default"
          onClick={() => void signIn()}
          loading={phase === "starting"}
          disabled={phase === "waiting"}
        >
          {phase === "signed-in" || editing ? tr("Sign in again") : tr("Sign in with AWS SSO")}
        </Button>
        {phase === "waiting" ? (
          <Button variant="subtle" onClick={cancel}>
            {tr("Cancel sign-in")}
          </Button>
        ) : null}
      </Group>
      {phase === "waiting" && started !== null ? (
        <Alert color="blue" variant="light" title={tr("Confirm the sign-in in your browser")} role="status">
          <Stack gap={4}>
            <Text size="sm">
              {tr("Enter this code if the page asks for it:")} <Code fz="md">{started.user_code}</Code>
            </Text>
            <Text size="sm">
              {started.browser_opened ? tr("The page was opened in your browser.") : tr("Open this page in your browser:")}
              <Anchor href={target} target="_blank" rel="noreferrer">
                {` ${started.verification_uri}`}
              </Anchor>
            </Text>
            <Text size="xs" c="dimmed">
              {tr("Waiting for you to approve...")}
            </Text>
          </Stack>
        </Alert>
      ) : null}
      {phase === "signed-in" ? (
        <Alert color="green" variant="light" role="status">
          {tr("Signed in. The token is kept in the AWS CLI cache (~/.aws/sso/cache), not in BucketBridge.")}
        </Alert>
      ) : null}
      {failure !== null ? (
        <Alert color="red" variant="light" role="alert">
          {failure}
        </Alert>
      ) : null}
      {editing ? (
        <Text size="sm">
          {tr("Account {account}, role {role}.", { account: draft.sso_account_id, role: draft.sso_role_name })}{" "}
          {tr("To use another role, add a new account.")}
        </Text>
      ) : (
        <>
          <Select
            label={tr("AWS account")}
            data={accounts.map((item) => ({
              value: item.account_id,
              label: `${item.account_name} (${item.account_id})`,
            }))}
            value={draft.sso_account_id === "" ? null : draft.sso_account_id}
            onChange={(value) => {
              if (value !== null) {
                void loadRoles(value);
              }
            }}
            error={errors.sso_account_id}
            placeholder={phase === "signed-in" ? tr("Choose an account") : tr("Sign in first")}
            disabled={phase !== "signed-in"}
            allowDeselect={false}
            comboboxProps={{ withinPortal: false }}
            searchable
          />
          <Select
            label={tr("Role")}
            data={roles}
            value={draft.sso_role_name === "" ? null : draft.sso_role_name}
            onChange={(value) => onChange({ sso_role_name: value ?? "" })}
            error={errors.sso_role_name}
            placeholder={draft.sso_account_id === "" ? tr("Choose an account first") : tr("Choose a role")}
            disabled={roles.length === 0}
            allowDeselect={false}
            comboboxProps={{ withinPortal: false }}
          />
        </>
      )}
    </Stack>
  );
}

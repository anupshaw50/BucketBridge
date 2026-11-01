import { PasswordInput, Text, TextInput } from "@mantine/core";
import { IconLock, IconShieldLock } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel, useSlice } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";

function UnlockDialog() {
  const model = useModel();
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await model.raw.unlock({ password });
      await model.session.refreshSecurity().catch(() => null);
      model.status.info("Secrets unlocked.");
      model.prompts.finishUnlock(true);
    } catch (raw) {
      const failure = toBbError(raw);
      setError(failure.code === "auth" ? "The master password is not correct. Try again." : failure.message);
      setPassword("");
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={
        <>
          <IconLock size={16} style={{ verticalAlign: "-2px", marginRight: 6 }} aria-hidden />
          Unlock secrets
        </>
      }
      okLabel="Unlock"
      onOk={() => void submit()}
      onCancel={() => model.prompts.finishUnlock(false)}
      okDisabled={password.length === 0}
      busy={busy}
      error={error}
      size="sm"
    >
      <Text size="sm">Stored credentials are protected by a master password. Enter it to use your accounts.</Text>
      <PasswordInput
        label="Master password"
        value={password}
        onChange={(event) => setPassword(event.currentTarget.value)}
        autoComplete="current-password"
        data-autofocus
      />
    </OkCancelDialog>
  );
}

function MfaDialog({ accountId }: { accountId: string }) {
  const model = useModel();
  const account = useSlice(model.session.store, (state) => state.accounts.find((item) => item.id === accountId));
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const valid = /^\d{6}$/.test(code);
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await model.api.mfa_login({ account_id: accountId, code });
      model.status.info(`MFA session started for ${account?.label ?? "the account"}.`);
      model.prompts.finishMfa(true);
    } catch (raw) {
      setError(toBbError(raw).message);
      setCode("");
    } finally {
      setBusy(false);
    }
  };
  return (
    <OkCancelDialog
      opened
      title={
        <>
          <IconShieldLock size={16} style={{ verticalAlign: "-2px", marginRight: 6 }} aria-hidden />
          Multi-factor authentication
        </>
      }
      okLabel="Verify"
      onOk={() => void submit()}
      onCancel={() => model.prompts.finishMfa(false)}
      okDisabled={!valid}
      busy={busy}
      error={error}
      size="sm"
    >
      <Text size="sm">
        {account ? (
          <>
            <strong>{account.label}</strong> requires a code from its MFA device
            {account.mfa_serial ? ` (${account.mfa_serial})` : ""}.
          </>
        ) : (
          "This account requires a code from its MFA device."
        )}
      </Text>
      <TextInput
        label="MFA code"
        description="Six digits"
        value={code}
        onChange={(event) => setCode(event.currentTarget.value.replace(/\D/g, "").slice(0, 6))}
        inputMode="numeric"
        autoComplete="one-time-code"
        maxLength={6}
        data-autofocus
      />
    </OkCancelDialog>
  );
}

/** Unlock and MFA prompts requested by the session guard. */
export function SessionPrompts() {
  const model = useModel();
  const unlockOpen = useSlice(model.prompts.store, (state) => state.unlockOpen);
  const mfaAccountId = useSlice(model.prompts.store, (state) => state.mfaAccountId);
  return (
    <>
      {unlockOpen ? <UnlockDialog /> : null}
      {mfaAccountId !== null ? <MfaDialog key={mfaAccountId} accountId={mfaAccountId} /> : null}
    </>
  );
}

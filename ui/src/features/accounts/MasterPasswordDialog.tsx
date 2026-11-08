import {
  Button,
  Checkbox,
  Group,
  NumberInput,
  Paper,
  PasswordInput,
  Progress,
  SegmentedControl,
  Stack,
  Text,
} from "@mantine/core";
import { IconWand } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel, useSlice } from "../../app/context";
import { tr } from "../../i18n";
import { CopyField } from "../../components/CopyField";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import {
  estimateStrength,
  generatePassword,
  PASSWORD_MAX_LENGTH,
  PASSWORD_MIN_LENGTH,
  type StrengthScore,
} from "../../lib/password";

const STRENGTH_COLORS: Record<StrengthScore, string> = { 0: "red", 1: "orange", 2: "yellow", 3: "teal", 4: "green" };

/** Strength meter with a short hint, shown under new-password fields. */
export function StrengthMeter({ password }: { password: string }) {
  const strength = estimateStrength(password);
  return (
    <Stack gap={2} aria-live="polite">
      <Progress
        value={password.length === 0 ? 0 : (strength.score + 1) * 20}
        color={STRENGTH_COLORS[strength.score]}
        size="xs"
        aria-label={tr("Password strength: {label}", { label: strength.label })}
      />
      <Text size="xs" c="dimmed">
        {tr("Strength")}: {strength.label}
        {strength.hint ? `. ${strength.hint}` : ""}
      </Text>
    </Stack>
  );
}

/** Password generator: length, symbols, crypto.getRandomValues, copy. */
export function PasswordGenerator({ onUse }: { onUse: (password: string) => void }) {
  const [length, setLength] = useState(20);
  const [symbols, setSymbols] = useState(true);
  const [generated, setGenerated] = useState("");
  const validLength = Number.isInteger(length) && length >= PASSWORD_MIN_LENGTH && length <= PASSWORD_MAX_LENGTH;
  return (
    <Paper withBorder p="sm">
      <Stack gap="xs">
        <Text size="sm" fw={600}>
          {tr("Password generator")}
        </Text>
        <Group align="flex-end" gap="sm">
          <NumberInput
            label={tr("Length")}
            w={110}
            min={PASSWORD_MIN_LENGTH}
            max={PASSWORD_MAX_LENGTH}
            allowDecimal={false}
            value={length}
            onChange={(value) => setLength(typeof value === "number" ? value : Number.NaN)}
            error={validLength ? null : `${PASSWORD_MIN_LENGTH} to ${PASSWORD_MAX_LENGTH}`}
          />
          <Checkbox
            label={tr("Include symbols")}
            checked={symbols}
            onChange={(event) => setSymbols(event.currentTarget.checked)}
            mb={8}
          />
          <Button
            variant="light"
            leftSection={<IconWand size={14} />}
            disabled={!validLength}
            onClick={() => setGenerated(generatePassword({ length, symbols }))}
          >
            {tr("Generate")}
          </Button>
        </Group>
        {generated.length > 0 ? (
          <Group align="flex-end" gap="sm" wrap="nowrap">
            <div style={{ flex: 1 }}>
              <CopyField label={tr("Generated password")} value={generated} sensitive />
            </div>
            <Button variant="default" onClick={() => onUse(generated)}>
              {tr("Use this password")}
            </Button>
          </Group>
        ) : null}
      </Stack>
    </Paper>
  );
}

type Mode = "change" | "remove";

/** Sets, changes or removes the master password that protects stored secrets. */
export function MasterPasswordDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const security = useSlice(model.session.store, (state) => state.security);
  const isSet = security?.master_password_set ?? false;
  const [mode, setMode] = useState<Mode>("change");
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [touched, setTouched] = useState(false);
  const removing = isSet && mode === "remove";
  const nextError =
    !removing && touched && next.length < PASSWORD_MIN_LENGTH
      ? tr("Use at least {n} characters.", { n: PASSWORD_MIN_LENGTH })
      : null;
  const confirmError = !removing && touched && confirm !== next ? tr("The passwords do not match.") : null;
  const currentError = isSet && touched && current.length === 0 ? tr("Enter the current master password.") : null;
  const valid =
    (!isSet || current.length > 0) && (removing || (next.length >= PASSWORD_MIN_LENGTH && confirm === next));

  const submit = async () => {
    setTouched(true);
    if (!valid) {
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await model.raw.set_master_password({
        current: isSet ? current : null,
        new_password: removing ? null : next,
      });
      await model.session.refreshSecurity().catch(() => null);
      model.status.info(
        removing
          ? tr("Master password removed.")
          : isSet
            ? tr("Master password changed.")
            : tr("Master password set."),
      );
      onClose();
    } catch (raw) {
      const failure = toBbError(raw);
      setError(failure.code === "auth" ? tr("The current master password is not correct.") : failure.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={isSet ? tr("Master password") : tr("Set a master password")}
      okLabel={removing ? tr("Remove password") : isSet ? tr("Change password") : tr("Set password")}
      danger={removing}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm">
        {tr(
          "The master password encrypts the stored account secrets. You enter it once per session. If you forget it, the secrets cannot be recovered and the accounts have to be added again.",
        )}
      </Text>
      {isSet ? (
        <SegmentedControl<Mode>
          value={mode}
          onChange={setMode}
          data={[
            { value: "change", label: tr("Change") },
            { value: "remove", label: tr("Remove") },
          ]}
          aria-label={tr("Action")}
        />
      ) : null}
      {isSet ? (
        <PasswordInput
          label={tr("Current master password")}
          value={current}
          onChange={(event) => setCurrent(event.currentTarget.value)}
          error={currentError}
          autoComplete="current-password"
          data-autofocus
        />
      ) : null}
      {!removing ? (
        <>
          <PasswordInput
            label={tr("New master password")}
            value={next}
            onChange={(event) => setNext(event.currentTarget.value)}
            error={nextError}
            autoComplete="new-password"
            data-autofocus={isSet ? undefined : true}
          />
          <StrengthMeter password={next} />
          <PasswordInput
            label={tr("Repeat the new password")}
            value={confirm}
            onChange={(event) => setConfirm(event.currentTarget.value)}
            error={confirmError}
            autoComplete="new-password"
          />
          <PasswordGenerator
            onUse={(password) => {
              setNext(password);
              setConfirm(password);
            }}
          />
        </>
      ) : (
        <Text size="sm" c="dimmed">
          {tr("Without a master password the secrets stay protected by the operating system's credential store only.")}
        </Text>
      )}
    </OkCancelDialog>
  );
}

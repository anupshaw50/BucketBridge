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
        aria-label={`Password strength: ${strength.label}`}
      />
      <Text size="xs" c="dimmed">
        Strength: {strength.label}
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
          Password generator
        </Text>
        <Group align="flex-end" gap="sm">
          <NumberInput
            label="Length"
            w={110}
            min={PASSWORD_MIN_LENGTH}
            max={PASSWORD_MAX_LENGTH}
            allowDecimal={false}
            value={length}
            onChange={(value) => setLength(typeof value === "number" ? value : Number.NaN)}
            error={validLength ? null : `${PASSWORD_MIN_LENGTH} to ${PASSWORD_MAX_LENGTH}`}
          />
          <Checkbox
            label="Include symbols"
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
            Generate
          </Button>
        </Group>
        {generated.length > 0 ? (
          <Group align="flex-end" gap="sm" wrap="nowrap">
            <div style={{ flex: 1 }}>
              <CopyField label="Generated password" value={generated} sensitive />
            </div>
            <Button variant="default" onClick={() => onUse(generated)}>
              Use this password
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
      ? `Use at least ${PASSWORD_MIN_LENGTH} characters.`
      : null;
  const confirmError = !removing && touched && confirm !== next ? "The passwords do not match." : null;
  const currentError = isSet && touched && current.length === 0 ? "Enter the current master password." : null;
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
        removing ? "Master password removed." : isSet ? "Master password changed." : "Master password set.",
      );
      onClose();
    } catch (raw) {
      const failure = toBbError(raw);
      setError(failure.code === "auth" ? "The current master password is not correct." : failure.message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <OkCancelDialog
      opened
      title={isSet ? "Master password" : "Set a master password"}
      okLabel={removing ? "Remove password" : isSet ? "Change password" : "Set password"}
      danger={removing}
      onOk={() => void submit()}
      onCancel={onClose}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm">
        The master password encrypts the stored account secrets. You enter it once per session. If you forget it, the
        secrets cannot be recovered and the accounts have to be added again.
      </Text>
      {isSet ? (
        <SegmentedControl<Mode>
          value={mode}
          onChange={setMode}
          data={[
            { value: "change", label: "Change" },
            { value: "remove", label: "Remove" },
          ]}
          aria-label="Action"
        />
      ) : null}
      {isSet ? (
        <PasswordInput
          label="Current master password"
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
            label="New master password"
            value={next}
            onChange={(event) => setNext(event.currentTarget.value)}
            error={nextError}
            autoComplete="new-password"
            data-autofocus={isSet ? undefined : true}
          />
          <StrengthMeter password={next} />
          <PasswordInput
            label="Repeat the new password"
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
          Without a master password the secrets stay protected by the operating system's credential store only.
        </Text>
      )}
    </OkCancelDialog>
  );
}

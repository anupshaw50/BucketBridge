import { Alert, Button, Group, Stack, Stepper } from "@mantine/core";
import { IconAlertCircle } from "@tabler/icons-react";
import type { FormEvent, ReactNode } from "react";

export interface WizardStep {
  label: string;
  content: ReactNode;
}

export interface WizardProps {
  steps: WizardStep[];
  active: number;
  /** Called by Next; the wizard only advances when it returns true. */
  onNext: () => boolean;
  onBack: () => void;
  onFinish: () => void;
  onCancel: () => void;
  finishLabel?: string;
  /** Label of the Next button (for example "Analyse" when Next starts work). */
  nextLabel?: string;
  cancelLabel?: string;
  /** Hides the Cancel button (when Finish already closes). */
  showCancel?: boolean;
  /** Disables Back, for example while work started by the last step runs. */
  backDisabled?: boolean;
  busy?: boolean;
  error?: string | null;
}

/** Back / Next / Finish wizard body; Enter moves to the next step or finishes. */
export function Wizard({
  steps,
  active,
  onNext,
  onBack,
  onFinish,
  onCancel,
  finishLabel = "Finish",
  nextLabel = "Next",
  cancelLabel = "Cancel",
  showCancel = true,
  backDisabled = false,
  busy = false,
  error = null,
}: WizardProps) {
  const last = active === steps.length - 1;
  const submit = (event: FormEvent) => {
    event.preventDefault();
    if (busy) {
      return;
    }
    if (last) {
      onFinish();
    } else {
      onNext();
    }
  };
  const current = steps[active];
  return (
    <form onSubmit={submit} noValidate>
      <Stack gap="md">
        <Stepper active={active} size="xs" allowNextStepsSelect={false}>
          {steps.map((step) => (
            <Stepper.Step key={step.label} label={step.label} />
          ))}
        </Stepper>
        <div role="group" aria-label={current?.label}>
          {current?.content}
        </div>
        {error ? (
          <Alert color="red" icon={<IconAlertCircle size={16} />} role="alert" py="xs">
            {error}
          </Alert>
        ) : null}
        <Group justify="space-between">
          {showCancel ? (
            <Button variant="default" onClick={onCancel}>
              {cancelLabel}
            </Button>
          ) : (
            <span />
          )}
          <Group gap="xs">
            <Button variant="default" onClick={onBack} disabled={active === 0 || busy || backDisabled}>
              Back
            </Button>
            {last ? (
              <Button type="submit" loading={busy}>
                {finishLabel}
              </Button>
            ) : (
              <Button type="submit" loading={busy}>
                {nextLabel}
              </Button>
            )}
          </Group>
        </Group>
      </Stack>
    </form>
  );
}

import { Text, TextInput } from "@mantine/core";
import { useState } from "react";
import type { ConfirmOptions } from "../state/dialogs";
import { tr } from "../i18n";
import { OkCancelDialog } from "./OkCancelDialog";

export interface ConfirmDialogProps {
  options: ConfirmOptions;
  onAnswer: (confirmed: boolean) => void;
}

/**
 * Yes/no confirmation. With `typedConfirmation` the user has to type the
 * given text (for example a bucket name) before the action is enabled.
 */
export function ConfirmDialog({ options, onAnswer }: ConfirmDialogProps) {
  const [typed, setTyped] = useState("");
  const required = options.typedConfirmation;
  const matches = required === undefined || typed === required;
  return (
    <OkCancelDialog
      opened
      title={options.title}
      okLabel={options.confirmLabel}
      danger={options.danger ?? false}
      okDisabled={!matches}
      onOk={() => onAnswer(true)}
      onCancel={() => onAnswer(false)}
    >
      <Text size="sm" style={{ whiteSpace: "pre-wrap", overflowWrap: "anywhere" }}>
        {options.message}
      </Text>
      {required !== undefined ? (
        <TextInput
          label={
            <>
              {tr("Type")} <strong style={{ overflowWrap: "anywhere" }}>{required}</strong> {tr("to confirm")}
            </>
          }
          value={typed}
          onChange={(event) => setTyped(event.currentTarget.value)}
          autoComplete="off"
          spellCheck={false}
          data-autofocus
        />
      ) : null}
    </OkCancelDialog>
  );
}

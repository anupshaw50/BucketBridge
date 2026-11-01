import { PasswordInput, Select, Stack, Text, TextInput } from "@mantine/core";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { CseCompression } from "../../api/types";
import { useModel } from "../../app/context";
import { OkCancelDialog } from "../../components/OkCancelDialog";
import { plural } from "../../lib/format";
import { passwordProblem } from "./problems";

const COMPRESSIONS: { value: CseCompression; label: string }[] = [
  { value: "none", label: "No compression" },
  { value: "gzip", label: "gzip" },
  { value: "zstd", label: "zstd" },
];

/** Encrypts or decrypts objects in place with a password (client-side, AES-256-GCM). */
export function EncryptionDialog({
  mode,
  accountId,
  bucket,
  keys,
  onClose,
}: {
  mode: "encrypt" | "decrypt";
  accountId: string;
  bucket: string;
  keys: string[];
  onClose: () => void;
}) {
  const model = useModel();
  const encrypt = mode === "encrypt";
  const word = encrypt ? "ENCRYPT" : "DECRYPT";
  const [password, setPassword] = useState("");
  const [again, setAgain] = useState("");
  const [compression, setCompression] = useState<CseCompression>("none");
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [failures, setFailures] = useState<string[]>([]);
  const mismatch = encrypt && again !== "" && again !== password;
  const ready = password !== "" && (!encrypt || again === password) && typed === word;

  const run = async () => {
    setBusy(true);
    setError(null);
    setFailures([]);
    const failed: string[] = [];
    let wrongPassword: string | null = null;
    for (const key of keys) {
      try {
        if (encrypt) {
          await model.api.cse_encrypt_object({ account_id: accountId, bucket, key, password, compression });
        } else {
          await model.api.cse_decrypt_object({ account_id: accountId, bucket, key, password });
        }
      } catch (raw) {
        const message = toBbError(raw).message;
        wrongPassword ??= passwordProblem(message);
        failed.push(`${key}: ${message}`);
        if (wrongPassword !== null) {
          break;
        }
      }
    }
    setBusy(false);
    if (failed.length === 0) {
      onClose();
      return;
    }
    setError(wrongPassword ?? `${plural(failed.length, "object")} could not be ${encrypt ? "encrypted" : "decrypted"}.`);
    setFailures(wrongPassword === null ? failed : []);
  };

  return (
    <OkCancelDialog
      opened
      title={encrypt ? "Encrypt objects" : "Decrypt objects"}
      okLabel={encrypt ? "Encrypt" : "Decrypt"}
      onOk={() => void run()}
      onCancel={onClose}
      okDisabled={!ready}
      busy={busy}
      error={error}
      size="lg"
    >
      <Text size="sm">
        {encrypt ? "Encrypts" : "Decrypts"} {plural(keys.length, "object")} in bucket <strong>{bucket}</strong> on this
        computer and replaces {keys.length === 1 ? "it" : "them"} under the same {keys.length === 1 ? "key" : "keys"}.
        The password is never stored; without it the data cannot be recovered.
      </Text>
      <PasswordInput label="Password" value={password} disabled={busy} onChange={(e) => setPassword(e.currentTarget.value)} />
      {encrypt ? (
        <>
          <PasswordInput
            label="Repeat password"
            value={again}
            disabled={busy}
            error={mismatch ? "The passwords differ." : null}
            onChange={(e) => setAgain(e.currentTarget.value)}
          />
          <Select
            label="Compression"
            value={compression}
            allowDeselect={false}
            disabled={busy}
            data={COMPRESSIONS}
            onChange={(value) => {
              if (value === "none" || value === "gzip" || value === "zstd") {
                setCompression(value);
              }
            }}
          />
        </>
      ) : null}
      <TextInput
        label={`Type ${word} to confirm`}
        value={typed}
        disabled={busy}
        onChange={(e) => setTyped(e.currentTarget.value)}
      />
      {failures.length > 0 ? (
        <Stack gap={2}>
          {failures.map((line) => (
            <Text key={line} size="xs" c="red" style={{ overflowWrap: "anywhere" }}>
              {line}
            </Text>
          ))}
        </Stack>
      ) : null}
    </OkCancelDialog>
  );
}

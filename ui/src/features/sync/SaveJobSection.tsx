import { Alert, Button, Group, Text, TextInput } from "@mantine/core";
import { IconDeviceFloppy } from "@tabler/icons-react";
import { useState } from "react";
import { toBbError } from "../../api/errors";
import type { SyncJob } from "../../api/types";
import { useModel } from "../../app/context";

const MAX_NAME = 100;

/**
 * "Save as job": names the setup of the sync dialog and stores it. With `existing` (editing a job) the
 * same button replaces that job. `build` returns the job from the form, or an explanation of what is missing.
 */
export function SaveJobSection({
  existing,
  build,
  onSaved,
}: {
  existing: SyncJob | null;
  build: (name: string) => SyncJob | string;
  onSaved?: (job: SyncJob) => void;
}) {
  const model = useModel();
  const [name, setName] = useState(existing?.name ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [savedName, setSavedName] = useState<string | null>(null);
  const editing = existing !== null && existing.id !== "";

  const save = async () => {
    setError(null);
    setSavedName(null);
    const trimmed = name.trim();
    if (trimmed === "") {
      setError("Enter a name for the job.");
      return;
    }
    if ([...trimmed].length > MAX_NAME) {
      setError(`Use at most ${MAX_NAME} characters.`);
      return;
    }
    const job = build(trimmed);
    if (typeof job === "string") {
      setError(job);
      return;
    }
    setBusy(true);
    try {
      const saved = await model.sync.saveJob(editing ? { ...job, id: existing.id } : job);
      setSavedName(saved.name);
      onSaved?.(saved);
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Group align="flex-end" gap="xs" wrap="nowrap" role="group" aria-label="Save as job">
      <TextInput
        label={editing ? "Job name" : "Save as job"}
        placeholder="Name, for example Nightly photos"
        value={name}
        maxLength={MAX_NAME * 2}
        style={{ flex: 1 }}
        error={error}
        onChange={(event) => setName(event.currentTarget.value)}
        onKeyDown={(event) => {
          // Enter must not move the wizard on.
          if (event.key === "Enter") {
            event.preventDefault();
            void save();
          }
        }}
      />
      <Button variant="default" leftSection={<IconDeviceFloppy size={14} />} loading={busy} onClick={() => void save()}>
        {editing ? "Save job" : "Save as job"}
      </Button>
      {savedName !== null ? (
        <Alert color="teal" py={4} px="xs" role="status">
          <Text size="xs">Saved job {savedName}.</Text>
        </Alert>
      ) : null}
    </Group>
  );
}

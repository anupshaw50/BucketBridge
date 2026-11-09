import { NumberInput, Stack, Switch, Text, Textarea, TextInput, UnstyledButton } from "@mantine/core";
import { IconChevronDown, IconChevronRight } from "@tabler/icons-react";
import { useState } from "react";
import { tr } from "../../i18n";
import type { SyncOptionsState } from "./syncOptionsState";

/** The filters and analysis options of a sync, folded away until opened. */
export function SyncOptionsForm({
  value,
  onChange,
}: {
  value: SyncOptionsState;
  onChange: (next: SyncOptionsState) => void;
}) {
  const [open, setOpen] = useState(false);
  const set = (patch: Partial<SyncOptionsState>) => onChange({ ...value, ...patch });
  return (
    <Stack gap="xs">
      <UnstyledButton
        onClick={() => setOpen(!open)}
        aria-expanded={open}
        aria-controls="bb-sync-options"
        style={{ display: "flex", alignItems: "center", gap: 6 }}
      >
        {open ? <IconChevronDown size={14} aria-hidden /> : <IconChevronRight size={14} aria-hidden />}
        <Text size="sm" fw={500}>
          {tr("Filters and options")}
        </Text>
      </UnstyledButton>
      {open ? (
        <Stack gap="sm" pl="md" id="bb-sync-options">
          <Textarea
            label={tr("Exclude rules")}
            description={tr("One rule per line, gitignore style: # comments, ! to include again, a trailing / for folders, ** across folders.")}
            placeholder={"*.tmp\nbuild/\n!keep.tmp"}
            rows={3}
            value={value.excludeText}
            onChange={(event) => set({ excludeText: event.currentTarget.value })}
          />
          <Textarea
            label={tr("Include only")}
            description={tr("Optional globs, one per line. When given, only matching files are synchronised.")}
            placeholder="**/*.jpg"
            rows={2}
            value={value.includeText}
            onChange={(event) => set({ includeText: event.currentTarget.value })}
          />
          <TextInput
            label={tr("Exclusion file")}
            description={tr("Optional path of a file with exclude rules, read every time the job runs.")}
            value={value.excludeFile}
            onChange={(event) => set({ excludeFile: event.currentTarget.value })}
          />
          <Switch
            label={tr("Skip empty folders")}
            description={tr("Empty folders are ignored. Turn off to create folder markers in S3 for empty local folders (and empty local folders for markers).")}
            checked={value.skipEmptyFolders}
            onChange={(event) => set({ skipEmptyFolders: event.currentTarget.checked })}
          />
          <Switch
            label={tr("Skip duplicate content")}
            description={tr("Treat a file as equal when its size and checksum match, even if the modification times differ.")}
            checked={value.skipDuplicateContent}
            onChange={(event) => set({ skipDuplicateContent: event.currentTarget.checked })}
          />
          <Switch
            label={tr("Use the local metadata cache")}
            description={tr("Reuse a recent listing of the S3 folder. Never used when extra files are deleted.")}
            checked={value.useCache}
            onChange={(event) => set({ useCache: event.currentTarget.checked })}
          />
          <NumberInput
            label={tr("Parallel listing")}
            description={tr("How many S3 folders are listed at the same time (1 to 32).")}
            min={1}
            max={32}
            allowDecimal={false}
            clampBehavior="strict"
            value={value.parallelism}
            onChange={(next) => set({ parallelism: typeof next === "number" ? next : 4 })}
            w={160}
          />
        </Stack>
      ) : null}
    </Stack>
  );
}

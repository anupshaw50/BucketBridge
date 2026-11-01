import { ActionIcon, Button, Group, Loader, ScrollArea, Stack, Text, Tooltip, UnstyledButton } from "@mantine/core";
import { IconArrowUp, IconFolder } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { toBbError } from "../../api/errors";
import { useModel } from "../../app/context";
import { isFolderEntry } from "../../lib/objects";
import { baseName, displaySegment, parentPrefix, s3Uri } from "../../lib/s3keys";

interface FolderPage {
  /** Location the page belongs to; a mismatch with the current location means loading. */
  key: string;
  folders: string[];
  nextToken: string | null;
  error: string | null;
}

export interface S3FolderPickerProps {
  accountId: string;
  bucket: string;
  prefix: string;
  onChange: (prefix: string) => void;
}

/** Browses the folders of a bucket; the open folder is the selected prefix. */
export function S3FolderPicker({ accountId, bucket, prefix, onChange }: S3FolderPickerProps) {
  const model = useModel();
  const locationKey = JSON.stringify([accountId, bucket, prefix]);
  const [page, setPage] = useState<FolderPage | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  useEffect(() => {
    let active = true;
    model.api
      .list_objects({ account_id: accountId, bucket, prefix, flat: false, token: null })
      .then(
        (result) => {
          if (active) {
            setPage({
              key: locationKey,
              folders: result.entries.filter((entry) => isFolderEntry(entry)).map((entry) => entry.key),
              nextToken: result.next_token,
              error: null,
            });
          }
        },
        (error: unknown) => {
          if (active) {
            setPage({ key: locationKey, folders: [], nextToken: null, error: toBbError(error).message });
          }
        },
      );
    return () => {
      active = false;
    };
  }, [model, accountId, bucket, prefix, locationKey]);
  const current = page?.key === locationKey ? page : null;
  const loadMore = async () => {
    const token = current?.nextToken ?? null;
    if (token === null) {
      return;
    }
    setLoadingMore(true);
    try {
      const result = await model.api.list_objects({
        account_id: accountId,
        bucket,
        prefix,
        flat: false,
        token,
      });
      setPage((latest) =>
        latest?.key === locationKey
          ? {
              ...latest,
              folders: [
                ...latest.folders,
                ...result.entries.filter((entry) => isFolderEntry(entry)).map((entry) => entry.key),
              ],
              nextToken: result.next_token,
            }
          : latest,
      );
    } catch (error) {
      model.status.error(error);
    } finally {
      setLoadingMore(false);
    }
  };
  return (
    <Stack gap={6}>
      <Group gap="xs" wrap="nowrap">
        <Tooltip label="Parent folder">
          <ActionIcon
            variant="default"
            size="sm"
            aria-label="S3 parent folder"
            disabled={prefix === ""}
            onClick={() => onChange(parentPrefix(prefix))}
          >
            <IconArrowUp size={14} />
          </ActionIcon>
        </Tooltip>
        <Text size="sm" ff="monospace" style={{ overflowWrap: "anywhere" }} aria-label="Selected S3 folder">
          {s3Uri(bucket, prefix)}
        </Text>
      </Group>
      <ScrollArea h={160} type="auto" className="bb-picker-list">
        {current === null ? (
          <Group p="xs" gap="xs">
            <Loader size={14} />
            <Text size="xs" c="dimmed">
              Loading folders...
            </Text>
          </Group>
        ) : current.error !== null ? (
          <Text size="xs" c="red" p="xs" role="alert">
            {current.error}
          </Text>
        ) : (
          <Stack gap={0} role="list" aria-label="Subfolders">
            {current.folders.length === 0 ? (
              <Text size="xs" c="dimmed" p="xs">
                No subfolders.
              </Text>
            ) : null}
            {current.folders.map((folder) => (
              <UnstyledButton
                key={folder}
                role="listitem"
                className="bb-picker-item"
                onClick={() => onChange(folder)}
                title={folder}
              >
                <IconFolder size={14} aria-hidden />
                <Text size="sm" className="bb-ellipsis">
                  {displaySegment(baseName(folder))}
                </Text>
              </UnstyledButton>
            ))}
            {current.nextToken !== null ? (
              <Button size="compact-xs" variant="subtle" loading={loadingMore} onClick={() => void loadMore()} m={4}>
                Load more folders
              </Button>
            ) : null}
          </Stack>
        )}
      </ScrollArea>
    </Stack>
  );
}

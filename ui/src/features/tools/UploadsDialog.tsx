import { Alert, Button, Checkbox, Group, MultiSelect, NumberInput, Stack, Table, Text } from "@mantine/core";
import { IconRefresh } from "@tabler/icons-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { toBbError } from "../../api/errors";
import type { AbortResult, UploadListing } from "../../api/types";
import { useModel, useSlice } from "../../app/context";
import { AppModal } from "../../components/AppModal";
import { formatDateTime, plural } from "../../lib/format";
import { formatAge, uploadId, uploadsOlderThan } from "../../lib/tools";

const DEFAULT_DAYS = 30;

/** An abort that left nothing behind: done, or the upload was already gone. */
function isGone(result: AbortResult): boolean {
  return result.aborted || result.error?.code === "not_found";
}

/**
 * Lists the uncompleted multipart uploads of the chosen buckets (they keep their uploaded parts, and
 * the storage they use is billed, until they are aborted) and aborts the ones that are selected.
 */
export function UploadsDialog({ onClose }: { onClose: () => void }) {
  const model = useModel();
  const accountId = useSlice(model.session.store, (state) => state.activeAccountId);
  const bucketList = useSlice(model.browser.store, (state) => state.bucketList);
  const openBucket = useSlice(model.browser.store, (state) => state.listing.bucket);
  const names = bucketList.accountId === accountId ? bucketList.buckets.map((bucket) => bucket.name) : [];
  const [chosen, setChosen] = useState<string[]>(() => (openBucket !== null && names.includes(openBucket) ? [openBucket] : []));
  const [listing, setListing] = useState<UploadListing | null>(null);
  const [listedAt, setListedAt] = useState(() => Date.now());
  const [loading, setLoading] = useState(false);
  const [aborting, setAborting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [picked, setPicked] = useState<ReadonlySet<string>>(new Set());
  const [days, setDays] = useState<number | string>(DEFAULT_DAYS);
  const [results, setResults] = useState<AbortResult[] | null>(null);

  const list = useCallback(
    async (buckets: string[]) => {
      if (accountId === null || buckets.length === 0) {
        return;
      }
      setLoading(true);
      setError(null);
      setResults(null);
      try {
        const next = await model.api.list_multipart_uploads({ account_id: accountId, buckets });
        setListing(next);
        setListedAt(Date.now());
        setPicked(new Set());
      } catch (raw) {
        setError(toBbError(raw).message);
      } finally {
        setLoading(false);
      }
    },
    [model, accountId],
  );

  // A bucket that is open when the tool starts is listed at once.
  const started = useRef(false);
  useEffect(() => {
    if (!started.current) {
      started.current = true;
      void list(chosen);
    }
  }, [list, chosen]);

  const rows = listing?.uploads ?? [];
  const allPicked = rows.length > 0 && rows.every((row) => picked.has(uploadId(row)));
  const somePicked = rows.some((row) => picked.has(uploadId(row)));
  const dayCount = typeof days === "number" ? days : Number(days);
  const olderValid = days !== "" && Number.isFinite(dayCount) && dayCount >= 0;

  const toggle = (id: string, on: boolean) => {
    setPicked((current) => {
      const next = new Set(current);
      if (on) {
        next.add(id);
      } else {
        next.delete(id);
      }
      return next;
    });
  };
  const selectOlder = () => {
    if (olderValid) {
      setPicked(new Set(uploadsOlderThan(rows, dayCount, listedAt).map((row) => uploadId(row))));
    }
  };

  const abort = async () => {
    const targets = rows.filter((row) => picked.has(uploadId(row)));
    if (accountId === null || targets.length === 0) {
      return;
    }
    const confirmed = await model.dialogs.confirm({
      title: "Abort uploads",
      message: `Abort ${plural(targets.length, "uncompleted upload")}? The parts they uploaded so far are deleted and their storage is freed. This cannot be undone.`,
      confirmLabel: "Abort uploads",
      danger: true,
    });
    if (!confirmed) {
      return;
    }
    setAborting(true);
    setError(null);
    try {
      const outcome = await model.api.abort_multipart_uploads({
        account_id: accountId,
        uploads: targets.map((row) => ({ bucket: row.bucket, key: row.key, upload_id: row.upload_id })),
      });
      const gone = new Set(outcome.filter((result) => isGone(result)).map((result) => uploadId(result)));
      setResults(outcome);
      setListing((current) =>
        current === null ? current : { ...current, uploads: current.uploads.filter((row) => !gone.has(uploadId(row))) },
      );
      setPicked((current) => new Set([...current].filter((id) => !gone.has(id))));
      const aborted = outcome.filter((result) => result.aborted).length;
      const failed = outcome.filter((result) => !isGone(result)).length;
      model.status.info(`${plural(aborted, "upload")} aborted${failed > 0 ? `, ${failed} could not be aborted` : ""}.`);
    } catch (raw) {
      setError(toBbError(raw).message);
    } finally {
      setAborting(false);
    }
  };

  return (
    <AppModal opened onClose={onClose} title="Uncompleted multipart uploads" size="xl" closeOnClickOutside={false}>
      {accountId === null ? (
        <Stack gap="sm">
          <Text size="sm">Add or choose an account first.</Text>
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Close
            </Button>
          </Group>
        </Stack>
      ) : (
        <Stack gap="sm">
          <Text size="sm">
            An upload that was started and never finished keeps the parts it uploaded, and they are billed until the upload is
            aborted. Choose buckets to see their unfinished uploads.
          </Text>
          <Group align="flex-end" gap="xs" wrap="nowrap">
            <MultiSelect
              label="Buckets"
              placeholder={chosen.length === 0 ? "Choose buckets" : undefined}
              data={names}
              value={chosen}
              onChange={setChosen}
              searchable
              nothingFoundMessage="No bucket with that name"
              style={{ flex: 1 }}
              comboboxProps={{ withinPortal: false }}
            />
            <Button variant="default" disabled={names.length === 0} onClick={() => setChosen(names)}>
              All buckets
            </Button>
            <Button
              leftSection={<IconRefresh size={14} />}
              loading={loading}
              disabled={chosen.length === 0}
              onClick={() => void list(chosen)}
            >
              {listing === null ? "List uploads" : "Refresh"}
            </Button>
          </Group>
          {error !== null ? (
            <Alert color="red" role="alert" py="xs">
              {error}
            </Alert>
          ) : null}
          {listing?.failures.map((failure) => (
            <Alert key={failure.bucket} color="red" role="alert" py="xs" title={`Could not list ${failure.bucket}`}>
              {failure.error.message}
            </Alert>
          ))}
          {listing !== null ? (
            rows.length === 0 ? (
              <Text size="sm" c="dimmed" aria-live="polite">
                No uncompleted uploads in the chosen buckets.
              </Text>
            ) : (
              <>
                <Group gap="xs" align="flex-end" wrap="nowrap">
                  <NumberInput
                    label="Older than (days)"
                    value={days}
                    onChange={setDays}
                    min={0}
                    max={36500}
                    allowDecimal={false}
                    allowNegative={false}
                    w={160}
                  />
                  <Button variant="default" disabled={!olderValid} onClick={selectOlder}>
                    Select older
                  </Button>
                  <Text size="sm" c="dimmed" aria-live="polite" style={{ flex: 1 }}>
                    {plural(picked.size, "upload")} selected of {rows.length}
                  </Text>
                  <Button color="red" disabled={picked.size === 0} loading={aborting} onClick={() => void abort()}>
                    Abort selected ({picked.size})
                  </Button>
                </Group>
                <div className="bb-upload-scroll">
                  <Table aria-label="Uncompleted uploads" stickyHeader verticalSpacing={4} fz="sm" highlightOnHover>
                    <Table.Thead>
                      <Table.Tr>
                        <Table.Th w={36}>
                          <Checkbox
                            aria-label="Select all uploads"
                            checked={allPicked}
                            indeterminate={somePicked && !allPicked}
                            onChange={(event) =>
                              setPicked(event.currentTarget.checked ? new Set(rows.map((row) => uploadId(row))) : new Set())
                            }
                          />
                        </Table.Th>
                        <Table.Th>Bucket</Table.Th>
                        <Table.Th>Key</Table.Th>
                        <Table.Th>Initiated</Table.Th>
                        <Table.Th>Age</Table.Th>
                        <Table.Th>Storage class</Table.Th>
                      </Table.Tr>
                    </Table.Thead>
                    <Table.Tbody>
                      {rows.map((row) => {
                        const id = uploadId(row);
                        return (
                          <Table.Tr key={id} bg={picked.has(id) ? "var(--mantine-color-blue-light)" : undefined}>
                            <Table.Td>
                              <Checkbox
                                aria-label={`Select ${row.key} in ${row.bucket}`}
                                checked={picked.has(id)}
                                onChange={(event) => toggle(id, event.currentTarget.checked)}
                              />
                            </Table.Td>
                            <Table.Td>{row.bucket}</Table.Td>
                            <Table.Td style={{ overflowWrap: "anywhere" }}>{row.key}</Table.Td>
                            <Table.Td style={{ whiteSpace: "nowrap" }}>{formatDateTime(row.initiated)}</Table.Td>
                            <Table.Td style={{ whiteSpace: "nowrap" }}>{formatAge(row.initiated, listedAt)}</Table.Td>
                            <Table.Td>{row.storage_class ?? ""}</Table.Td>
                          </Table.Tr>
                        );
                      })}
                    </Table.Tbody>
                  </Table>
                </div>
              </>
            )
          ) : null}
          {results !== null ? (
            <Stack gap={4}>
              <Text size="sm" fw={600}>
                Results of the last abort
              </Text>
              <div className="bb-upload-scroll">
                <Table aria-label="Abort results" stickyHeader verticalSpacing={4} fz="sm">
                  <Table.Thead>
                    <Table.Tr>
                      <Table.Th>Bucket</Table.Th>
                      <Table.Th>Key</Table.Th>
                      <Table.Th>Result</Table.Th>
                    </Table.Tr>
                  </Table.Thead>
                  <Table.Tbody>
                    {results.map((result) => (
                      <Table.Tr key={uploadId(result)}>
                        <Table.Td>{result.bucket}</Table.Td>
                        <Table.Td style={{ overflowWrap: "anywhere" }}>{result.key}</Table.Td>
                        <Table.Td c={result.error === null ? undefined : "red"} style={{ overflowWrap: "anywhere" }}>
                          {result.error === null ? "Aborted" : result.error.message}
                        </Table.Td>
                      </Table.Tr>
                    ))}
                  </Table.Tbody>
                </Table>
              </div>
            </Stack>
          ) : null}
          <Group justify="flex-end">
            <Button variant="default" onClick={onClose}>
              Close
            </Button>
          </Group>
        </Stack>
      )}
    </AppModal>
  );
}

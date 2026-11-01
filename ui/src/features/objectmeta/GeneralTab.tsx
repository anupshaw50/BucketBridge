import { Stack, Table, Text } from "@mantine/core";
import type { ReactNode } from "react";
import type { ObjectEntry } from "../../api/types";
import { CopyField } from "../../components/CopyField";
import { formatBytes, formatCount, formatDateTime, plural } from "../../lib/format";
import { baseName, displaySegment, s3Uri } from "../../lib/s3keys";
import type { TabContext } from "./apply";

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <Table.Tr>
      <Table.Th scope="row" w={150} fw={500} style={{ verticalAlign: "top" }}>
        {label}
      </Table.Th>
      <Table.Td style={{ overflowWrap: "anywhere" }}>{children}</Table.Td>
    </Table.Tr>
  );
}

function Mono({ children }: { children: ReactNode }) {
  return (
    <Text size="sm" ff="monospace">
      {children}
    </Text>
  );
}

/** Read-only facts about one object, or a summary of the selection. */
export function GeneralTab({ ctx, entries }: { ctx: TabContext; entries: readonly ObjectEntry[] }) {
  const details = ctx.details;
  if (details === null) {
    const total = entries.reduce((sum, entry) => sum + entry.size, 0);
    const classes = new Map<string, number>();
    for (const entry of entries) {
      const name = entry.storage_class ?? "Unknown";
      classes.set(name, (classes.get(name) ?? 0) + 1);
    }
    return (
      <Stack gap="xs">
        <Text size="sm">
          {plural(entries.length, "object")} in <strong>{ctx.bucket}</strong>, {formatBytes(total)} in total.
        </Text>
        <Text size="sm" c="dimmed">
          Storage classes: {[...classes].map(([name, count]) => `${name} (${count})`).join(", ")}.
        </Text>
        <Text size="sm" c="dimmed">
          The other tabs change all of them at once and show the result for each object.
        </Text>
      </Stack>
    );
  }
  const checksums = Object.entries(details.checksums).filter((pair): pair is [string, string] => pair[1] !== null);
  return (
    <Stack gap="sm">
      <Table withTableBorder verticalSpacing={4} fz="sm">
        <Table.Tbody>
          <Row label="Name">{displaySegment(baseName(details.key))}</Row>
          <Row label="Type">Object</Row>
          <Row label="Bucket">{ctx.bucket}</Row>
          <Row label="Key">
            <Mono>{details.key}</Mono>
          </Row>
          <Row label="Size">
            {formatBytes(details.size)} ({formatCount(details.size)} bytes)
          </Row>
          <Row label="Last modified">
            {details.last_modified ? `${formatDateTime(details.last_modified)} (${details.last_modified})` : "Unknown"}
          </Row>
          <Row label="Storage class">{details.storage_class ?? "Unknown"}</Row>
          <Row label="ETag">
            <Mono>{details.etag ?? "None"}</Mono>
          </Row>
          {details.version_id !== null ? (
            <Row label="Version ID">
              <Mono>{details.version_id}</Mono>
            </Row>
          ) : null}
          {details.restore !== null ? (
            <Row label="Restore">
              {details.restore.in_progress
                ? "A restore is in progress."
                : `Restored copy available${details.restore.expires ? ` until ${formatDateTime(details.restore.expires)}` : ""}.`}
            </Row>
          ) : null}
          {checksums.length > 0 ? (
            <Row label="Checksums">
              <Stack gap={2}>
                {checksums.map(([algorithm, value]) => (
                  <Mono key={algorithm}>
                    {algorithm.toUpperCase()}: {value}
                  </Mono>
                ))}
              </Stack>
            </Row>
          ) : null}
        </Table.Tbody>
      </Table>
      <CopyField label="S3 URI" value={s3Uri(ctx.bucket, details.key)} />
    </Stack>
  );
}

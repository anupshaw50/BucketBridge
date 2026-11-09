import { Alert, Text } from "@mantine/core";
import { IconInfoCircle } from "@tabler/icons-react";
import type { PreviewTable } from "../../api/types";
import { tr } from "../../i18n";
import { formatCount } from "../../lib/format";

export interface PreviewTableViewProps {
  table: PreviewTable;
  /** Index of the first row of this page, from 0. */
  offset: number;
  label: string;
}

/** One page of a CSV, TSV or Parquet table. Cells are text, never markup. */
export function PreviewTableView({ table, offset, label }: PreviewTableViewProps) {
  return (
    <>
      {table.truncated ? (
        <Alert color="yellow" icon={<IconInfoCircle size={16} />} mb="xs" role="status">
          {tr("This file is larger than the preview limit; only its first {rows} rows are shown.", {
            rows: formatCount(table.total_rows),
          })}
        </Alert>
      ) : null}
      {table.columns_cut ? (
        <Alert color="yellow" icon={<IconInfoCircle size={16} />} mb="xs" role="status">
          {tr("Only the first {count} columns are shown.", { count: formatCount(table.columns.length) })}
        </Alert>
      ) : null}
      <div className="bb-preview-tablewrap" tabIndex={0} role="region" aria-label={label}>
        <table className="bb-preview-table">
          <thead>
            <tr>
              <th scope="col" className="bb-preview-rownum">
                {"#"}
              </th>
              {table.columns.map((name, index) => (
                <th scope="col" key={index} title={name}>
                  {name === "" ? tr("Column {n}", { n: index + 1 }) : name}
                </th>
              ))}
            </tr>
          </thead>
          <tbody>
            {table.rows.map((row, rowIndex) => (
              <tr key={rowIndex}>
                <td className="bb-preview-rownum">{formatCount(offset + rowIndex + 1)}</td>
                {table.columns.map((_, cellIndex) => {
                  const cell = row[cellIndex] ?? null;
                  return cell === null ? (
                    <td key={cellIndex} data-null="">
                      {tr("null")}
                    </td>
                  ) : (
                    <td key={cellIndex} title={cell}>
                      {cell}
                    </td>
                  );
                })}
              </tr>
            ))}
          </tbody>
        </table>
        {table.rows.length === 0 ? (
          <Text size="sm" c="dimmed" p="sm">
            {tr("This table has no rows.")}
          </Text>
        ) : null}
      </div>
    </>
  );
}

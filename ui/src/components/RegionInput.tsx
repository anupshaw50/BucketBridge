import { Autocomplete } from "@mantine/core";
import { useMemo } from "react";
import type { PartitionId } from "../api/types";
import { tr } from "../i18n";
import { useRegions } from "../lib/useRegions";

export interface RegionInputProps {
  label: string;
  description?: string;
  placeholder?: string;
  value: string;
  onChange: (value: string) => void;
  error?: string | null;
  required?: boolean;
  /** Only suggest regions of this partition (an AWS account cannot reach another partition). */
  partition?: PartitionId | null;
  /** Regions of an S3-compatible provider; when given they replace the AWS table. */
  extraRegions?: string[];
  "data-autofocus"?: boolean;
}

/** A region field with suggestions from the shared table; any other name can still be typed. */
export function RegionInput({ partition = null, extraRegions = [], description, value, onChange, ...rest }: RegionInputProps) {
  const regions = useRegions();
  const suggestions = useMemo(
    () =>
      extraRegions.length > 0
        ? extraRegions
        : regions.filter((item) => partition === null || item.partition === partition).map((item) => item.id),
    [regions, partition, extraRegions],
  );
  const known = regions.find((item) => item.id === value.trim());
  const shown = known
    ? tr("{name}, partition {partition}.", { name: known.name, partition: known.partition })
    : description;
  return (
    <Autocomplete
      {...rest}
      description={shown}
      value={value}
      onChange={onChange}
      data={suggestions}
      limit={50}
      spellCheck={false}
      autoComplete="off"
    />
  );
}

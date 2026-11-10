import type { PartitionId, RegionEntry } from "../api/types";

/** The partition a region code belongs to (table first, then the name pattern). */
export function partitionOf(regions: RegionEntry[], region: string): PartitionId {
  const known = regions.find((item) => item.id === region);
  if (known) {
    return known.partition;
  }
  if (/^cn-[a-z0-9-]+-\d+$/.test(region)) {
    return "aws-cn";
  }
  if (/^us-gov-[a-z0-9-]+-\d+$/.test(region)) {
    return "aws-us-gov";
  }
  if (/^eusc-[a-z0-9-]+-\d+$/.test(region)) {
    return "aws-eusc";
  }
  return "aws";
}

/** ARN prefix of a region's partition, for templates such as `arn:aws-cn:s3:::bucket`. */
export function arnPrefix(regions: RegionEntry[], region: string): string {
  return `arn:${partitionOf(regions, region)}`;
}

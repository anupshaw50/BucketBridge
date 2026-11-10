import type { ArchiveKind, RestoreEstimate, RestoreSummary, RestoreTier } from "../api/types";

/** The date of the price table (kept in step with `PRICE_DATE` in crates/core/src/objectlock/estimate.rs). */
export const MOCK_PRICE_DATE = "2025-06-01";

const NOTE =
  "Approximate, us-east-1 list prices as of 2025-06-01; check your AWS bill. Taxes, discounts, other regions and the storage of the restored copy are not included.";

const GB = 1_073_741_824;

/** USD per GB and per 1,000 requests; a missing entry means the tier cannot restore that archive. */
const PRICES: Record<ArchiveKind, Partial<Record<RestoreTier, [number, number]>>> = {
  glacier: { expedited: [0.03, 10], standard: [0.01, 0.05], bulk: [0, 0.025] },
  deep_archive: { standard: [0.02, 0.1], bulk: [0.0025, 0.025] },
  intelligent_archive: { standard: [0, 0], bulk: [0, 0] },
  intelligent_deep_archive: { standard: [0, 0], bulk: [0, 0] },
};

/** Storage classes of the mock that stand for the Intelligent-Tiering archive tiers. */
export const MOCK_IT_ARCHIVE = "IT_ARCHIVE_ACCESS";
export const MOCK_IT_DEEP_ARCHIVE = "IT_DEEP_ARCHIVE_ACCESS";

function kindOf(storageClass: string): ArchiveKind | null {
  switch (storageClass) {
    case "GLACIER":
      return "glacier";
    case "DEEP_ARCHIVE":
      return "deep_archive";
    case MOCK_IT_ARCHIVE:
      return "intelligent_archive";
    case MOCK_IT_DEEP_ARCHIVE:
      return "intelligent_deep_archive";
    default:
      return null;
  }
}

/** The mock twin of `estimate_restore`: `objects` has one entry per key, `null` for an object that cannot be read. */
export function estimateRestore(objects: ({ class: string; size: number } | null)[]): RestoreEstimate {
  const summary: RestoreSummary = { groups: [], not_archived: 0, unreadable: 0, truncated: false };
  for (const object of objects) {
    if (object === null) {
      summary.unreadable += 1;
      continue;
    }
    const kind = kindOf(object.class);
    if (kind === null) {
      summary.not_archived += 1;
      continue;
    }
    const group = summary.groups.find((entry) => entry.kind === kind);
    if (group === undefined) {
      summary.groups.push({ kind, objects: 1, bytes: object.size });
    } else {
      group.objects += 1;
      group.bytes += object.size;
    }
  }
  const tiers = (["expedited", "standard", "bulk"] as const).map((tier) => {
    let retrieval = 0;
    let requests = 0;
    let count = 0;
    let unsupported = 0;
    for (const group of summary.groups) {
      const price = PRICES[group.kind][tier];
      if (price === undefined) {
        unsupported += group.objects;
      } else {
        retrieval += (group.bytes / GB) * price[0];
        requests += (group.objects / 1000) * price[1];
        count += group.objects;
      }
    }
    return {
      tier,
      total_usd: retrieval + requests,
      retrieval_usd: retrieval,
      requests_usd: requests,
      objects: count,
      unsupported,
    };
  });
  return {
    summary,
    objects: summary.groups.reduce((sum, group) => sum + group.objects, 0),
    bytes: summary.groups.reduce((sum, group) => sum + group.bytes, 0),
    tiers,
    price_date: MOCK_PRICE_DATE,
    note: NOTE,
  };
}

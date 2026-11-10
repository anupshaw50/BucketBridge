import type { ArchiveState, RestoreEstimate, RestoreTier, TierEstimate } from "../../api/types";
import { tr } from "../../i18n";

/** US dollars for an estimate: "$0.00" for nothing, four decimals below one cent, else two. */
export function formatUsd(value: number): string {
  if (value > 0 && value < 0.0001) {
    return "<$0.0001";
  }
  if (value > 0 && value < 0.01) {
    return `$${value.toFixed(4)}`;
  }
  return `$${value.toFixed(2)}`;
}

/** Whether every archived object of an estimate sits in an Intelligent-Tiering archive tier (no days are needed). */
export function onlyIntelligentTiering(estimate: RestoreEstimate): boolean {
  return (
    estimate.summary.groups.length > 0 &&
    estimate.summary.groups.every((group) => group.kind === "intelligent_archive" || group.kind === "intelligent_deep_archive")
  );
}

/** The display name of a tier. */
export function tierName(tier: RestoreTier): string {
  switch (tier) {
    case "expedited":
      return tr("Expedited");
    case "standard":
      return tr("Standard");
    default:
      return tr("Bulk");
  }
}

/** The line that shows the estimate of one tier. */
export function tierEstimateText(entry: TierEstimate): string {
  const name = tierName(entry.tier);
  if (entry.objects === 0 && entry.unsupported > 0) {
    return tr("{tier}: not available for these objects.", { tier: name });
  }
  const cost = tr("{tier}: about {cost}", { tier: name, cost: formatUsd(entry.total_usd) });
  return entry.unsupported > 0 ? tr("{text} ({n} objects cannot use this tier)", { text: cost, n: entry.unsupported }) : cost;
}

/** Text for the restore state of an archived object. */
export function restoreText(state: ArchiveState): string {
  if (state.restore === null) {
    return state.archived
      ? tr("Archived ({class}), not restored.", { class: state.storage_class })
      : tr("Not archived ({class}).", { class: state.storage_class });
  }
  if (state.restore.in_progress) {
    return tr("A restore is in progress.");
  }
  return state.restore.expires === null
    ? tr("Restored.")
    : tr("Restored until {time}.", { time: state.restore.expires });
}

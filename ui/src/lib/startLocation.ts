import type { WindowLocation } from "../api/types";

/**
 * The location a window opened with "Open in new window" starts at, from the query of its URL
 * (`bb_account`, `bb_bucket`, `bb_prefix`); null for a normal start or an incomplete query.
 */
export function startLocationFromSearch(search: string): WindowLocation | null {
  const params = new URLSearchParams(search);
  const account = params.get("bb_account");
  const bucket = params.get("bb_bucket");
  if (account === null || account === "" || bucket === null || bucket === "") {
    return null;
  }
  return { account_id: account, bucket, prefix: params.get("bb_prefix") ?? "" };
}

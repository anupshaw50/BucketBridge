import type { CloudFrontView } from "../../state/dialogs";
import type { AppModel } from "../../state/model";

/** Opens the CloudFront manager for an account, optionally narrowed to the distributions of a bucket. */
export function showCloudFront(model: AppModel, accountId: string, bucket: string | null, view: CloudFrontView = "distributions"): void {
  model.dialogs.open({ kind: "cloudfront", accountId, bucket, view });
}

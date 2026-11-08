import type { BucketPropertiesTab } from "../../state/dialogs";
import type { AppModel } from "../../state/model";
import { tr } from "../../i18n";

/** Opens the properties of a bucket (default: the one open in the object browser) on a tab. */
export function showBucketProperties(
  model: AppModel,
  tab?: BucketPropertiesTab,
  at?: { accountId: string; bucket: string },
): void {
  const { accountId, bucket } = at ?? model.browser.state.listing;
  if (accountId !== null && bucket !== null) {
    model.dialogs.open({ kind: "bucket-properties", accountId, bucket, ...(tab === undefined ? {} : { tab }) });
  }
}

/** Opens the dialog that adds an external bucket to the active account. */
export function showAddExternalBucket(model: AppModel): void {
  const accountId = model.session.store.getState().activeAccountId;
  if (accountId !== null) {
    model.dialogs.open({ kind: "external-bucket", accountId });
  }
}

/**
 * Removes an external bucket from the bucket list of an account after a confirmation. Only the entry
 * goes: the bucket itself is not touched, and it can be added again.
 */
export async function removeExternalBucket(model: AppModel, accountId: string, name: string): Promise<void> {
  const confirmed = await model.dialogs.confirm({
    title: tr("Remove external bucket"),
    message: tr("Remove {name} from the bucket list of this account?\n\nThe bucket itself is not changed, and you can add it again later.", { name }),
    confirmLabel: tr("Remove"),
  });
  if (!confirmed) {
    return;
  }
  try {
    await model.api.remove_external_bucket({ account_id: accountId, name });
    model.status.info(tr("External bucket {name} removed.", { name }));
  } catch (error) {
    model.status.error(error, { notify: tr("The external bucket could not be removed") });
  }
  if (model.browser.state.listing.bucket === name) {
    await model.browser.switchAccount(accountId);
  } else {
    await model.browser.loadBuckets(accountId);
  }
}

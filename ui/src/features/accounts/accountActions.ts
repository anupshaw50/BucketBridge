import type { Account } from "../../api/types";
import type { AppModel } from "../../state/model";

/** Removes an account and its stored secrets after the user types its label. */
export async function removeAccount(model: AppModel, account: Account): Promise<void> {
  const confirmed = await model.dialogs.confirm({
    title: "Remove account",
    message: `Remove "${account.label}" and delete its stored secrets? Transfers of this account that are still queued will fail.`,
    confirmLabel: "Remove account",
    danger: true,
    typedConfirmation: account.label,
  });
  if (!confirmed) {
    return;
  }
  try {
    await model.api.remove_account({ id: account.id });
    await model.session.refreshAccounts();
    model.status.info(`Account "${account.label}" removed.`);
  } catch (error) {
    model.status.error(error, { notify: "The account could not be removed" });
  }
}

/** Makes an account the one that is selected at startup. */
export async function makeDefault(model: AppModel, account: Account): Promise<void> {
  try {
    await model.api.set_default_account({ id: account.id });
    await model.session.refreshAccounts();
    model.status.info(`"${account.label}" is now the default account.`);
  } catch (error) {
    model.status.error(error, { notify: "The default account could not be changed" });
  }
}

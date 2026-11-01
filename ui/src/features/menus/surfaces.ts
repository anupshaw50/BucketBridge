import type { Account, Bucket, ObjectEntry } from "../../api/types";
import { t } from "../../i18n";
import { isFolderEntry } from "../../lib/objects";
import type { AppModel } from "../../state/model";
import {
  accountContext,
  bucketContext,
  bucketsEmptyContext,
  filesEmptyContext,
  itemsContext,
  type MenuContext,
} from "./context";
import type { MenuId, Pane } from "./ids";
import { type ResolvedItem, resolveMenu } from "./resolve";

/** A menu ready to open: its entries and its accessible name. */
export interface BuiltMenu {
  items: ResolvedItem[];
  label: string;
}

function build(menu: MenuId, ctx: MenuContext): BuiltMenu {
  return { items: resolveMenu(menu, ctx), label: t(`menu.${menu}`) };
}

/** Right-click on one or more buckets of the buckets pane. */
export function bucketMenu(model: AppModel, buckets: readonly Bucket[]): BuiltMenu {
  return build("bucket", bucketContext(model, buckets));
}

/** Right-click on a folder or file (or on several selected rows) of one bucket. */
export function itemsMenu(model: AppModel, bucket: string, entries: readonly ObjectEntry[], pane: Pane): BuiltMenu {
  const [first] = entries;
  let menu: MenuId = "selection";
  if (entries.length === 1 && first !== undefined) {
    menu = isFolderEntry(first) ? "folder" : "file";
  }
  return build(menu, itemsContext(model, bucket, entries, pane));
}

/** Right-click on the empty area of the object list. */
export function filesEmptyMenu(model: AppModel): BuiltMenu {
  return build("files-empty", filesEmptyContext(model));
}

/** Right-click on the empty area of the buckets pane. */
export function bucketsEmptyMenu(model: AppModel): BuiltMenu {
  return build("buckets-empty", bucketsEmptyContext(model));
}

/** Right-click on an account. */
export function accountMenu(model: AppModel, account: Account): BuiltMenu {
  return build("account", accountContext(model, account));
}

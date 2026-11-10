import { zhAbout } from "./about";
import { zhAccounts } from "./accounts";
import { zhApi } from "./api";
import { zhApp } from "./app";
import { zhBookmarks } from "./bookmarks";
import { zhBucketcfg } from "./bucketcfg";
import { zhBucketclone } from "./bucketclone";
import { zhBucketrules } from "./bucketrules";
import { zhBuckets } from "./buckets";
import { zhCloudfront } from "./cloudfront";
import { zhComponents } from "./components";
import { zhCrash } from "./crash";
import { zhCse } from "./cse";
import { zhDialogHost } from "./dialogHost";
import { zhEdit } from "./edit";
import { zhEveryday } from "./everyday";
import { zhFileopen } from "./fileopen";
import { zhIam } from "./iam";
import { zhLib } from "./lib";
import { zhLog } from "./log";
import { zhMenus } from "./menus";
import { zhMount } from "./mount";
import { zhObjectlock } from "./objectlock";
import { zhObjectmeta } from "./objectmeta";
import { zhObjects } from "./objects";
import { zhPicker } from "./picker";
import { zhScan } from "./scan";
import { zhSettings } from "./settings";
import { zhSharing } from "./sharing";
import { zhState } from "./state";
import { zhSync } from "./sync";
import { zhTools } from "./tools";
import { zhTransfers } from "./transfers";
import { zhUploadrules } from "./uploadrules";
import { zhVersions } from "./versions";

const PARTS: readonly Record<string, string>[] = [
  zhAbout,
  zhAccounts,
  zhApi,
  zhApp,
  zhBookmarks,
  zhBucketcfg,
  zhBucketclone,
  zhBucketrules,
  zhBuckets,
  zhCloudfront,
  zhComponents,
  zhCrash,
  zhCse,
  zhDialogHost,
  zhEdit,
  zhEveryday,
  zhFileopen,
  zhIam,
  zhLib,
  zhLog,
  zhMenus,
  zhMount,
  zhObjectlock,
  zhObjectmeta,
  zhObjects,
  zhPicker,
  zhScan,
  zhSettings,
  zhSharing,
  zhState,
  zhSync,
  zhTools,
  zhTransfers,
  zhUploadrules,
  zhVersions,
];

/** 简体中文文案总表：键为英文原文，值为中文译文（菜单文案在 ./menu.ts，另行合并）。 */
export const zh: Record<string, string> = {};
/** 被两处译成不同中文的英文原文；i18n 覆盖率测试对它报错。 */
export const conflicts: string[] = [];

for (const part of PARTS) {
  for (const [source, text] of Object.entries(part)) {
    const known: string | undefined = zh[source];
    if (known === undefined) {
      zh[source] = text;
    } else if (known !== text && !conflicts.includes(source)) {
      conflicts.push(source);
    }
  }
}

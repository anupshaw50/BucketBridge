/** 简体中文文案（everyday：退出确认、bb:// 链接、保持唤醒）。键是英文原文，值是中文译文。 */
export const zhEveryday: Record<string, string> = {
  // ---- features/quit/QuitDialog.tsx ----
  "Quit BucketBridge?": "退出 BucketBridge？",
  "1 transfer is running or waiting.": "有 1 个传输正在运行或排队。",
  "{n} transfers are running or waiting.": "有 {n} 个传输正在运行或排队。",
  "1 sync run is in progress.": "有 1 个同步正在进行。",
  "{n} sync runs are in progress.": "有 {n} 个同步正在进行。",
  "Quitting now stops running transfers; they stay in the queue and can be resumed. Running sync runs are cancelled.":
    "立即退出会停止正在运行的传输；它们会留在队列中，之后可以继续。正在进行的同步会被取消。",
  "Quit now": "立即退出",
  "Finish running tasks, then quit": "等任务完成后退出",

  // ---- state/model.ts（bb:// 链接）----
  "The link could not be opened": "无法打开该链接",

  // ---- features/settings/SettingsDialog.tsx ----
  "Keep the computer awake during transfers": "传输期间保持电脑唤醒",
  "Stops the computer from sleeping while transfers or a sync run work. It is released as soon as they end.":
    "传输或同步运行期间，阻止电脑进入睡眠；任务一结束就立即释放。",
  "When quitting while transfers are running": "有传输正在运行时退出",
  "Ask me what to do": "询问我怎么做",
  "Let running tasks finish, then quit": "让任务完成后再退出",
  "Quit at once (transfers can be resumed later)": "立即退出（传输之后可继续）",
};

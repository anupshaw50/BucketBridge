/** 简体中文文案（mount）。键是英文原文，值是中文译文。 */
export const zhMount: Record<string, string> = {
  // ---- MountDialog.tsx ----
  "Mount {bucket} as drive": "将 {bucket} 挂载为驱动器",
  "This build of BucketBridge cannot mount buckets. See docs/mount.md.": "此 BucketBridge 构建无法挂载存储桶。请参阅 docs/mount.md。",
  "Dokan is not installed. Install the Dokan 2 runtime (github.com/dokan-dev/dokany/releases) and try again.":
    "未安装 Dokan。请安装 Dokan 2 运行时（github.com/dokan-dev/dokany/releases）后重试。",
  "FUSE is not installed. Install macFUSE or FUSE-T (macOS) or fuse3 (Linux) and try again.":
    "未安装 FUSE。请安装 macFUSE 或 FUSE-T（macOS）或 fuse3（Linux）后重试。",
  "The bucket appears as a read-only {form} ({driver}). Nothing in it can be changed from there.":
    "存储桶将以只读的{form}（{driver}）形式出现。无法从中更改任何内容。",
  "drive or folder": "驱动器或文件夹",
  folder: "文件夹",
  "Drive letter": "驱动器盘符",
  "Pick a free drive letter": "请选择一个空闲的盘符",
  "Mount folder": "挂载文件夹",
  "A free drive letter such as X:, or an existing empty folder on an NTFS drive.": "空闲的盘符（例如 X:），或 NTFS 驱动器上已有的空文件夹。",
  "An existing, empty folder (absolute path).": "已有的空文件夹（绝对路径）。",
  "Browse...": "浏览…",
  "Prefix (optional)": "前缀（可选）",
  "Mount only this folder of the bucket.": "只挂载存储桶中的这个文件夹。",
  Mount: "挂载",
  "Active mounts": "活动挂载",
  Unmount: "卸载",
  "Mounted {bucket} on {point}.": "已将 {bucket} 挂载到 {point}。",
  "Unmounted {point}.": "已卸载 {point}。",
  "Choose the mount folder": "选择挂载文件夹",
  "The folder dialog could not be opened": "无法打开文件夹对话框",
};

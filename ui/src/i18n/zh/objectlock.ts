/** 简体中文文案（objectlock）。键是英文原文，值是中文译文。 */
export const zhObjectlock: Record<string, string> = {
  // ---- ObjectLockDialog.tsx（对象的保留与法定持有）----
  "Compliance retention": "合规保留",
  "Nobody, not even the root account, can shorten or remove a compliance retention before it ends. {n} will be locked.":
    "在合规保留到期之前，任何人都无法缩短或移除它，即使是根账户也不行。{n} 将被锁定。",
  "1 object": "1 个对象",
  "{n} objects": "{n} 个对象",
  Lock: "锁定",
  "Retention saved.": "保留设置已保存。",
  "Remove retention": "移除保留",
  "Remove the retention of 1 object?": "要移除这 1 个对象的保留吗？",
  "Remove the retention of {n} objects?": "要移除这 {n} 个对象的保留吗？",
  "{question} The governance bypass is used.": "{question}将使用治理绕过。",
  Remove: "移除",
  "Retention removed.": "保留设置已移除。",
  "Legal hold saved.": "法定持有已保存。",
  "Lock 1 object": "锁定 1 个对象",
  "Lock {n} objects": "锁定 {n} 个对象",
  "Lock: {key}": "锁定：{key}",
  "Set retention": "设置保留",
  "Save legal hold": "保存法定持有",
  "No retention": "无保留",
  "{mode} until {until}": "{mode}，保留至 {until}",
  ", legal hold on": "，法定持有开启",
  "Retention mode": "保留模式",
  Governance: "治理",
  Compliance: "合规",
  "Retain until": "保留至",
  "Date and time, for example 2030-01-31T12:00": "日期和时间，例如 2030-01-31T12:00",
  "Enter a valid date and time.": "请输入有效的日期和时间。",
  "Bypass governance retention": "绕过治理保留",
  "Legal hold": "法定持有",

  // ---- RestoreDialog.tsx（从 Glacier 恢复）----
  "Restore from Glacier": "从 Glacier 恢复",
  "Request restore": "请求恢复",
  "Restore requested for 1 object.": "已请求恢复 1 个对象。",
  "Restore requested for {n} objects.": "已请求恢复 {n} 个对象。",
  Tier: "恢复层级",
  Expedited: "快速",
  Standard: "标准",
  Bulk: "批量",
  "Keep the restored copy for (days)": "恢复副本保留天数",

  // ---- restoreText.ts ----
  "Archived ({class}), not restored.": "已归档（{class}），尚未恢复。",
  "Not archived ({class}).": "未归档（{class}）。",
  "A restore is in progress.": "恢复正在进行中。",
  "Restored.": "已恢复。",
  "Restored until {time}.": "已恢复，保留至 {time}。",

  // ---- BucketLockDialog.tsx（存储桶的对象锁定）----
  "Turn on Object Lock": "开启对象锁定",
  "Object Lock cannot be turned off again once it is on for {bucket}. Versioning must be enabled.":
    "对 {bucket} 开启后，对象锁定就无法再次关闭。必须先启用版本控制。",
  "Turn on": "开启",
  "Object Lock settings of {bucket} saved.": "已保存 {bucket} 的对象锁定设置。",
  "Object lock: {bucket}": "对象锁定：{bucket}",
  Save: "保存",
  "Reading...": "读取中…",
  "Object Lock is on for this bucket.": "此存储桶已开启对象锁定。",
  "Object Lock is off for this bucket.": "此存储桶未开启对象锁定。",
  "Default retention for new objects": "新对象的默认保留",
  "Retain for": "保留时长",
  Years: "年",

  // ---- OwnershipDialog.tsx（对象所有权）----
  "Object ownership: {bucket}": "对象所有权：{bucket}",
  "Disable ACLs": "禁用 ACL",
  "With BucketOwnerEnforced, ACLs of {bucket} stop working and requests that set them fail.":
    "在 BucketOwnerEnforced 下，{bucket} 的 ACL 不再生效，设置 ACL 的请求会失败。",
  "Object ownership of {bucket} saved.": "已保存 {bucket} 的对象所有权。",
  "Bucket owner enforced": "由存储桶所有者强制执行",
  "Bucket owner preferred": "优先由存储桶所有者持有",
  "Object writer": "由上传者持有",
  "ACLs are disabled; the bucket owner owns every object.": "ACL 被禁用；存储桶所有者拥有每个对象。",
  "The bucket owner owns objects uploaded with the bucket-owner-full-control ACL.":
    "以 bucket-owner-full-control ACL 上传的对象由存储桶所有者持有。",
  "The account that uploads an object owns it.": "上传对象的账户拥有该对象。",
  "No setting is configured; S3 behaves as Object writer.": "未配置该项；S3 的行为如同「由上传者持有」。",
};

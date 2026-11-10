/** 简体中文文案（克隆存储桶）。键是英文原文，值是中文译文。 */
export const zhBucketclone: Record<string, string> = {
  "Clone bucket: {bucket}": "克隆存储桶：{bucket}",
  "Clone bucket": "克隆存储桶",
  "Creates a new bucket with the settings you choose, and can copy the objects into it. The source bucket is not changed.":
    "创建一个带有所选设置的新存储桶，并可把对象复制进去。源存储桶不会被改动。",
  "New bucket name": "新存储桶名称",
  "Leave empty to use the region of the source{details}.": "留空则使用源存储桶所在的区域{details}。",
  "Reading the settings of the source…": "正在读取源存储桶的设置…",
  "Settings to copy": "要复制的设置",
  Setting: "设置",
  "On the source": "源存储桶上",
  "Default encryption": "默认加密",
  "Public access block": "公共访问阻止",
  "Static website hosting": "静态网站托管",
  "Bucket tags": "存储桶标签",
  "Lifecycle rules": "生命周期规则",
  "Not set on the source": "源存储桶未设置",
  "Not supported by this server": "此服务器不支持",
  "Could not be read: {message}": "无法读取：{message}",
  "Blocking {count} of 4": "已阻止 4 项中的 {count} 项",
  "Statements: {count}": "语句数：{count}",
  "Index document: {name}": "索引文档：{name}",
  "Rules: {count}": "规则数：{count}",
  "Tags: {count}": "标签数：{count}",
  "The policy has Deny statements. Settings are applied before the objects are copied, so a policy that denies you access can block the copy. Clear Bucket policy and copy it afterwards if the copy fails.":
    "该策略含有 Deny 语句。设置会在复制对象之前应用，因此拒绝你访问的策略可能会阻止复制。如果复制失败，请取消勾选存储桶策略，复制完成后再单独复制它。",
  "The default encryption uses a KMS key. The key must exist in the region of the new bucket (or be a multi-region key), or this setting is refused.":
    "默认加密使用了 KMS 密钥。该密钥必须存在于新存储桶所在的区域（或是多区域密钥），否则此设置会被拒绝。",
  "Versioning is on in the source. Only the current version of each object is copied, not the older versions.":
    "源存储桶已启用版本控制。只会复制每个对象的当前版本，不复制旧版本。",
  "Nothing to copy": "没有可复制的内容",
  "Failed: {message}": "失败：{message}",
  "{done} of {total} objects copied. The copy runs in the transfer queue, so you can close this window.":
    "已复制 {done} / {total} 个对象。复制在传输队列中进行，可以关闭此窗口。",
  "Copy progress": "复制进度",
  "{count} copies failed. Retry them from the transfers list.": "{count} 个复制失败。请在传输列表中重试。",
  "A prefix is a folder: it ends with /.": "前缀是一个文件夹：以 / 结尾。",
  "Copy the objects": "复制对象",
  "The objects are copied on the server by the transfer queue, with their keys unchanged. Older versions are not copied.":
    "对象由传输队列在服务器端复制，键保持不变。不复制旧版本。",
  "Only objects under this prefix (optional)": "仅复制此前缀下的对象（可选）",
  "Count objects": "统计对象",
  "{count} objects, {size}": "{count} 个对象，{size}",
  "What will happen: create {name}, copy {count} settings and the objects.":
    "将执行：创建 {name}，复制 {count} 项设置和对象。",
  "What will happen: create {name} and copy {count} settings.": "将执行：创建 {name}，并复制 {count} 项设置。",
  "Bucket {name} was created.": "存储桶 {name} 已创建。",
  "Settings copied": "已复制的设置",
  "The objects could not be queued": "无法将对象加入队列",
  "There are no objects to copy.": "没有要复制的对象。",
  "Open the new bucket": "打开新存储桶",
};

/** 简体中文文案（objectmeta）。键是英文原文，值是中文译文。 */
export const zhObjectmeta: Record<string, string> = {
  // ---- MetadataDialog.tsx（属性对话框）----
  Properties: "属性",
  "Properties of 1 object": "1 个对象的属性",
  "Properties of {n} objects": "{n} 个对象的属性",
  "Type of object": "类型",
  Folder: "文件夹",
  "A folder has no metadata of its own: tags, storage class, encryption, headers and ACLs belong to the objects in it. Select objects to edit them.":
    "文件夹本身没有元数据：标签、存储类型、加密、头字段和 ACL 都属于其中的对象。请选择对象来编辑它们。",
  "1 folder in the selection is left out: folders have no metadata of their own.":
    "所选内容中的 1 个文件夹已排除：文件夹本身没有元数据。",
  "{n} folders in the selection are left out: folders have no metadata of their own.":
    "所选内容中的 {n} 个文件夹已排除：文件夹本身没有元数据。",
  "Loading properties": "正在加载属性",
  "The properties could not be read": "无法读取属性",
  "Customer key (SSE-C)": "客户密钥（SSE-C）",
  "If the object is encrypted with a customer-provided key, enter it to read the object. It is never stored.":
    "如果对象是用客户提供的密钥加密的，请输入该密钥以读取对象。密钥不会被存储。",
  "Read with key": "使用密钥读取",
  "HTTP headers": "HTTP 头",
  "User metadata": "用户元数据",
  Tags: "标签",
  Permissions: "权限",

  // ---- GeneralTab.tsx（常规页）----
  Object: "对象",
  "{n} objects in {bucket}, {size} in total.": "{bucket} 中共 {n} 个对象，总计 {size}。",
  "{n} bytes": "{n} 字节",
  "{changed} changed, {unchanged} unchanged": "{changed} 个已更改，{unchanged} 个未更改",
  "{summary}, {failed} failed": "{summary}，{failed} 个失败",
  "1 object: {summary}.": "1 个对象：{summary}。",
  "{n} objects: {summary}.": "{n} 个对象：{summary}。",
  "1 object in {bucket}, {size} in total.": "{bucket} 中共 1 个对象，总计 {size}。",
  "Storage classes: {classes}.": "存储类型：{classes}。",
  "The other tabs change all of them at once and show the result for each object.":
    "其他页签会一次性更改全部对象，并显示每个对象的结果。",
  Unknown: "未知",
  None: "无",
  "Version ID": "版本 ID",
  Restore: "恢复",
  Checksums: "校验和",
  "Restored copy available until {time}.": "已有恢复副本，保留至 {time}。",
  "Restored copy available.": "已有恢复副本。",
  "A restored copy is available until {time}.": "已有恢复副本，保留至 {time}。",
  "A restored copy is available.": "已有恢复副本。",

  // ---- parts.tsx（应用栏与结果列表）----
  "Changed.": "已更改。",
  "Already up to date; nothing was changed.": "已无需更改；没有做任何修改。",
  Results: "结果",
  Result: "结果",
  "Failed: {message}": "失败：{message}",
  "Changed (was {previous})": "已更改（原为 {previous}）",
  Changed: "已更改",
  Unchanged: "未更改",
  "Key of objects encrypted with SSE-C": "以 SSE-C 加密对象的密钥",
  "Only for objects that are encrypted with a customer-provided key. It is sent with the request and never stored.":
    "仅适用于以客户提供的密钥加密的对象。密钥随请求发送，不会被存储。",

  // ---- EditTabs.tsx（HTTP 头、用户元数据、标签）----
  "for example text/plain; charset=utf-8": "例如 text/plain; charset=utf-8",
  "for example public, max-age=3600": "例如 public, max-age=3600",
  "for example attachment; filename=\"a.pdf\"": "例如 attachment; filename=\"a.pdf\"",
  "for example gzip": "例如 gzip",
  "Leave empty to keep": "留空则保留原值",
  "Fill in the headers to set on every selected object, or tick Remove. Empty fields leave each object's header as it is.":
    "填写要应用到每个所选对象上的头字段，或勾选「移除」。留空的字段会保留各对象原有的头字段。",
  "Clear a field to remove the header. S3 then falls back to its default (binary/octet-stream for Content-Type).":
    "清空字段即可移除该头字段。之后 S3 会回退到其默认值（Content-Type 为 binary/octet-stream）。",
  "Remove {label}": "移除 {label}",
  "Custom metadata is stored with the object as x-amz-meta-* headers. Keys are not case sensitive; all keys and values together can take 2,048 bytes.":
    "自定义元数据以 x-amz-meta-* 头字段的形式与对象一起存储。键不区分大小写；所有键和值合计最多 2,048 字节。",
  "Metadata key": "元数据键",
  "Metadata value": "元数据值",
  "Add metadata": "添加元数据",
  "Remove these keys": "移除这些键",
  "Separate keys with commas.": "用逗号分隔多个键。",
  "The tags could not be read": "无法读取标签",
  "Tags below are added to every selected object (an existing tag with the same key gets the new value). Other tags stay.":
    "下方标签会添加到每个所选对象（同键的现有标签会被新值覆盖）。其余标签保留。",
  "An object can have up to 10 tags. Keys and values may contain letters, digits, spaces and + - = . _ : / @.":
    "一个对象最多可有 10 个标签。键和值可包含字母、数字、空格和 + - = . _ : / @。",
  "Tag key": "标签键",
  "Tag value": "标签值",
  "Add tag": "添加标签",
  "Remove these tags": "移除这些标签",
  "Tag keys, separated by commas.": "标签键，用逗号分隔。",
  "Replace all existing tags with the ones above": "用上方标签替换全部现有标签",
  "{label} {n}": "{label} {n}",
  "Remove {label} {n}": "移除{label} {n}",

  // ---- SecurityTabs.tsx（存储类型、加密、权限）----
  "S3 changes the class by copying the object onto itself. Headers, user metadata, tags, encryption and the ACL are kept; the object gets a new modification time, and on a versioned bucket a new version.":
    "S3 通过把对象复制到自身来更改存储类型。头字段、用户元数据、标签、加密和 ACL 都会保留；对象会获得新的修改时间，在启用了版本控制的存储桶上还会获得新版本。",
  "This object is archived ({class}) and has not been restored. Restore it before changing its storage class.":
    "该对象已归档（{class}）且尚未恢复。请先恢复它，再更改其存储类型。",
  "Choose a class": "选择存储类型",
  "Not reported (the bucket's default applies).": "未报告（适用存储桶的默认设置）。",
  "SSE-S3: keys managed by S3.": "SSE-S3：密钥由 S3 管理。",
  ", key {id}": "，密钥为 {id}",
  ", AWS managed key": "，使用 AWS 托管密钥",
  "SSE-KMS{key}.": "SSE-KMS{key}。",
  "SSE-KMS{key}, with an S3 Bucket Key.": "SSE-KMS{key}，并使用 S3 存储桶密钥。",
  "SSE-C: encrypted with a customer-provided key (MD5 {md5}).": "SSE-C：已用客户提供的密钥加密（MD5 {md5}）。",
  "S3 re-encrypts by copying the object onto itself; headers, user metadata, tags, the storage class and the ACL are kept. Encryption cannot be removed, only changed.":
    "S3 通过把对象复制到自身来重新加密；头字段、用户元数据、标签、存储类型和 ACL 都会保留。加密无法移除，只能更换。",
  "Encryption type": "加密类型",
  "A KMS key has no spaces.": "KMS 密钥不能包含空格。",
  "KMS key": "KMS 密钥",
  "Key ID, key ARN, alias name or alias ARN. Leave empty for the AWS managed key (aws/s3).":
    "密钥 ID、密钥 ARN、别名或别名 ARN。留空则使用 AWS 托管密钥（aws/s3）。",
  "Use an S3 Bucket Key (fewer requests to KMS)": "使用 S3 存储桶密钥（减少对 KMS 的请求）",
  "New customer key": "新客户密钥",
  "32 bytes: 64 hexadecimal digits, or 44 base64 characters. It is sent with the request and never stored.":
    "32 字节：64 个十六进制数字，或 44 个 base64 字符。密钥随请求发送，不会被存储。",
  "Generate key": "生成密钥",
  "Copy key": "复制密钥",
  "Keep the key safe. Without it the object cannot be read, copied or changed, and BucketBridge cannot recover it.":
    "请妥善保管密钥。没有它，对象将无法读取、复制或修改，BucketBridge 也无法找回。",
  "The new key was copied. Keep it safe: it is the only way to read the objects again.":
    "已复制新密钥。请妥善保管：这是再次读取这些对象的唯一途径。",
  "The ACL could not be read": "无法读取 ACL",
  "Current encryption:": "当前加密：",
  "Current ACL:": "当前 ACL：",
  "custom grants": "自定义授权",
  Grants: "授权",
  Grantee: "被授权者",
  Permission: "权限",
  "Buckets with ACLs disabled (Object Ownership: BucketOwnerEnforced) reject changes here; use the bucket policy there. Public ACLs can also be blocked by the bucket's Block Public Access settings.":
    "禁用了 ACL 的存储桶（对象所有权：BucketOwnerEnforced）会拒绝此处的更改；请改用存储桶策略。公共 ACL 也可能被存储桶的公共访问封锁设置阻止。",
  "Canned ACL": "预设 ACL",
  "Choose an ACL": "选择一个 ACL",

  // ---- AclGrantsEditor.tsx, SecurityTabs.tsx ----
  "ACLs are disabled here (Object Ownership: BucketOwnerEnforced), so the grants below cannot be changed. Change the bucket's Object Ownership to let ACLs work, or control access with the bucket policy.":
    "此处已禁用 ACL（对象所有权：BucketOwnerEnforced），因此下面的授权无法更改。请修改存储桶的对象所有权以启用 ACL，或改用存储桶策略控制访问。",
  "Remove the grant {permission} for {who}": "移除 {who} 的 {permission} 授权",
  "Grantee type": "被授权者类型",
  Group: "组",
  "S3 accepts e-mail addresses in some regions only; the canonical account ID always works.":
    "S3 只在部分区域接受电子邮件地址；规范账户 ID 始终可用。",
  "The canonical user ID of the account, not the 12-digit account number.": "账户的规范用户 ID，而不是 12 位账户编号。",
  "Add grant": "添加授权",
  "This grant is already in the list.": "该授权已在列表中。",
  "These grants make the content reachable by people outside your account. The public access block may still stop them.":
    "这些授权会让账户以外的人也能访问内容。公共访问屏蔽仍可能阻止他们。",
  "Save grants": "保存授权",
  "The ACL grants of {key} were saved.": "{key} 的 ACL 授权已保存。",
};

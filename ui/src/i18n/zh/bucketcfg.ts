/** 简体中文文案（bucketcfg）。键是英文原文，值是中文译文。 */
export const zhBucketcfg: Record<string, string> = {
  // ---- actions.ts ----
  "Remove external bucket": "移除外部存储桶",
  "Remove {name} from the bucket list of this account?\n\nThe bucket itself is not changed, and you can add it again later.":
    "将 {name} 从该账户的存储桶列表中移除？\n\n存储桶本身不受影响，稍后可以重新添加。",
  Remove: "移除",
  "External bucket {name} removed.": "外部存储桶 {name} 已移除。",
  "The external bucket could not be removed": "无法移除外部存储桶",

  // ---- common.tsx ----
  "Access denied": "访问被拒绝",
  "Not found": "未找到",
  "This setting could not be read": "无法读取该设置",
  "Not available": "不可用",

  // ---- BucketPropertiesDialog.tsx ----
  "Bucket properties: {bucket}": "{bucket} 的存储桶属性",
  "Public access": "公共访问",
  Versioning: "版本控制",
  Logging: "日志",
  Website: "网站",
  Acceleration: "加速",
  "Loading the bucket settings": "正在读取存储桶设置",
  "The bucket settings could not be read": "无法读取存储桶设置",
  Close: "关闭",

  // ---- ExternalBucketDialog.tsx ----
  "Add external bucket": "添加外部存储桶",
  "Add bucket": "添加存储桶",
  "Bucket name": "存储桶名称",
  "Enter the name of the bucket.": "请输入存储桶名称。",
  "A bucket name has at most 255 characters.": "存储桶名称最多 255 个字符。",
  "Use letters, digits, dots, hyphens and underscores, starting with a letter or digit.":
    "只能使用字母、数字、点号、连字符和下划线，并以字母或数字开头。",
  "External bucket {name} added.": "外部存储桶 {name} 已添加。",
  "Use this for a bucket that you can open but that is missing from the list: your account is not allowed to list buckets, or the bucket belongs to another account. It is added to the list of this account and marked External.":
    "用于打开列表中缺失的存储桶：你的账户无权列出存储桶，或者该存储桶属于另一个账户。它会被添加到该账户的列表中，并标记为 External。",

  // ---- LoggingTab.tsx ----
  "Server access logging delivers a log of the requests made to this bucket as objects in another bucket.":
    "服务器访问日志会把对该存储桶的请求以对象的形式记录到另一个存储桶中。",
  "Log requests made to this bucket": "记录对该存储桶的请求",
  "Target bucket": "目标存储桶",
  "It must be in the same region and allow S3 to deliver logs.": "目标存储桶必须位于同一区域，并允许 S3 投递日志。",
  "Log prefix": "日志前缀",
  "Added in front of the name of every log object, for example logs/.": "加在每个日志对象名称的前面，例如 logs/。",
  "Enter the bucket that receives the logs.": "请输入接收日志的存储桶。",
  "Access logging of {bucket} was saved.": "已保存 {bucket} 的访问日志设置。",
  "Access logging of {bucket} was turned off.": "已关闭 {bucket} 的访问日志。",

  // ---- PolicyTab.tsx ----
  "The policy attached to this bucket. Edit it, then validate and save.": "附加到该存储桶的策略。编辑后校验并保存。",
  "This bucket has no policy. Write one below and save it.": "该存储桶还没有策略。请在下方编写并保存。",
  "Bucket policy (JSON)": "存储桶策略（JSON）",
  "The policy is valid.": "策略有效。",
  "1 problem": "1 个问题",
  "{n} problems": "{n} 个问题",
  Validate: "校验",
  Format: "格式化",
  "Save policy": "保存策略",
  "Delete policy": "删除策略",
  "Delete bucket policy": "删除存储桶策略",
  "The policy of {bucket} is deleted. Access to the bucket then depends only on its other settings, such as IAM policies and ACLs.\n\nThis cannot be undone.":
    "将删除 {bucket} 的策略。此后对存储桶的访问仅取决于它的其余设置，例如 IAM 策略和 ACL。\n\n此操作无法撤销。",
  "The policy of {bucket} was deleted.": "已删除 {bucket} 的策略。",
  "The policy of {bucket} was saved.": "已保存 {bucket} 的策略。",

  // ---- PublicAccessTab.tsx ----
  "No public access block is configured, so S3 blocks nothing on this bucket.":
    "未配置公共访问封锁，S3 不拦截对该存储桶的任何访问。",
  "These switches limit how public the bucket and its objects can become.": "这些开关限制存储桶及其对象能达到的公开程度。",
  "Block new public ACLs": "阻止新的公共 ACL",
  "Requests that would make the bucket or an object public through an ACL are rejected (BlockPublicAcls).":
    "会通过 ACL 将存储桶或对象设为公开的请求将被拒绝（BlockPublicAcls）。",
  "Ignore public ACLs": "忽略现有的公共 ACL",
  "Public ACLs that already exist have no effect (IgnorePublicAcls).": "已存在的公共 ACL 不再生效（IgnorePublicAcls）。",
  "Block new public bucket policies": "阻止新的公共存储桶策略",
  "A bucket policy that would make the bucket public is rejected (BlockPublicPolicy).":
    "会将存储桶设为公开的存储桶策略将被拒绝（BlockPublicPolicy）。",
  "Restrict access to buckets with a public policy": "限制对拥有公共策略的存储桶的访问",
  "A public policy only gives access to the owner's account and to AWS services (RestrictPublicBuckets).":
    "公共策略只向所有者的账户和 AWS 服务授予访问权限（RestrictPublicBuckets）。",
  "The public access block of {bucket} was saved.": "已保存 {bucket} 的公共访问封锁。",
  "Remove public access block": "移除公共访问封锁",
  "The public access block of {bucket} is removed, so S3 stops blocking public access to it. The bucket is then only as private as its ACLs and policy make it.":
    "移除 {bucket} 的公共访问封锁后，S3 不再阻止对它的公共访问。此后存储桶的私密程度只取决于它的 ACL 和策略。",
  "Remove configuration": "移除配置",
  "The public access block of {bucket} was removed.": "已移除 {bucket} 的公共访问封锁。",

  // ---- VersioningTab.tsx ----
  "Versioning has never been enabled on this bucket.": "该存储桶从未启用过版本控制。",
  "Versioning is enabled: every change to an object keeps the previous version.":
    "版本控制已启用：对对象的每次更改都会保留先前版本。",
  "Versioning is suspended: no new versions are kept, existing versions stay.":
    "版本控制已暂停：不再保留新版本，现有版本继续保留。",
  "MFA delete": "MFA 删除",
  Disabled: "已禁用",
  Enabled: "已启用",
  "Shown for information; it cannot be changed here.": "仅作展示；此处无法更改。",
  "Keep every version of every object.": "保留每个对象的全部版本。",
  "Stop creating new versions. Versioning cannot be switched off completely once it was enabled.":
    "不再创建新版本。版本控制一旦启用，就无法完全关闭。",
  Suspended: "已暂停",
  "Versioning of {bucket} is now {choice}.": "{bucket} 的版本控制现在为{choice}。",

  // ---- AccelerationTab.tsx ----
  "Transfer acceleration has never been configured on this bucket.": "该存储桶从未配置过传输加速。",
  "Transfer acceleration is enabled.": "传输加速已启用。",
  "Transfer acceleration is suspended.": "传输加速已暂停。",
  "Transfer acceleration of {bucket} is now {choice}.": "{bucket} 的传输加速现在为{choice}。",
  "Acceleration routes transfers over the AWS edge network, which helps over long distances. It is billed separately.":
    "传输加速经由 AWS 边缘网络路由传输，远距离传输更快。单独计费。",
  "Transfer acceleration": "传输加速",
  "Not available: the name of this bucket contains dots.": "不可用：该存储桶的名称包含点号。",

  // ---- WebsiteTab.tsx ----
  "This bucket does not serve a website.": "该存储桶没有托管网站。",
  "This bucket serves a static website.": "该存储桶托管一个静态网站。",
  "This bucket redirects every request to another host.": "该存储桶把所有请求重定向到另一台主机。",
  "Website mode": "网站模式",
  Off: "关闭",
  "Host a static website": "托管静态网站",
  "Redirect all requests": "重定向所有请求",
  "Index document": "索引文档",
  "Served for requests to a folder.": "请求文件夹时返回。",
  "Error document": "错误文档",
  "Optional. Served when a request fails.": "可选。请求失败时返回。",
  "Routing rules": "路由规则",
  "No routing rules. A rule redirects requests that match its condition.": "没有路由规则。规则会把符合其条件的请求重定向。",
  "Routing rule {number}": "路由规则 {number}",
  "Remove rule": "移除规则",
  "If the key starts with": "如果键以下列内容开头",
  "If the error code is": "如果错误代码为",
  "Redirect protocol": "重定向协议",
  "Redirect to host": "重定向到主机",
  "Replace the key prefix with": "把键前缀替换为",
  "Replace the whole key with": "把整个键替换为",
  "Redirect code": "重定向代码",
  "Add routing rule": "添加路由规则",
  "Host name": "主机名",
  "Every request goes to this host.": "所有请求都会发往该主机。",
  Protocol: "协议",
  "Same as the request": "与请求相同",
  "Check the website settings": "请检查网站设置",
  "Save website settings": "保存网站设置",
  "The website configuration of {bucket} was saved.": "已保存 {bucket} 的网站配置。",
  "Disable website hosting": "停用网站托管",
  "Website hosting of {bucket} is turned off. Its website endpoint stops serving pages, and the routing rules are deleted.\n\nThe objects in the bucket are not changed.":
    "将关闭 {bucket} 的网站托管。其网站端点不再提供页面，路由规则将被删除。\n\n存储桶中的对象不受影响。",
  "Disable hosting": "关闭托管",
  "Website hosting of {bucket} was turned off.": "已关闭 {bucket} 的网站托管。",

  // ---- UsageTab.tsx ----
  Usage: "用量",
  "Bucket usage": "存储桶用量",
  "{size} ({bytes} bytes)": "{size}（{bytes} 字节）",
  "Size of objects": "对象大小",
  "Non-current versions": "非当前版本",
  "Size of non-current versions": "非当前版本大小",
  "Delete markers": "删除标记",
  "Unfinished multipart uploads": "未完成的分段上传",
  "Size of unfinished uploads": "未完成上传的大小",
  "Adds up the size of the bucket by listing it. A bucket with many objects takes a while; you can cancel.":
    "通过列出对象来统计存储桶的大小。对象很多时需要一些时间，可以随时取消。",
  "Include all versions and delete markers": "包含所有版本和删除标记",
  "Versioning has never been enabled on this bucket, so there is nothing more to find.":
    "此存储桶从未启用版本控制，因此没有更多内容。",
  "Include unfinished multipart uploads": "包含未完成的分段上传",
  "Calculate size": "计算大小",
  "Calculate again": "重新计算",
  Calculating: "正在计算",
  "Sizing unfinished multipart uploads...": "正在统计未完成的分段上传……",
  "Counting objects: {count} so far": "已统计对象：{count}",
  "The size could not be calculated": "无法计算大小",
  "Cancelled. The totals are incomplete.": "已取消。合计不完整。",

  // ---- 动态取词（状态枚举、选项值经 tr() 查表）----
  enabled: "已启用",
  suspended: "已暂停",

  // ---- 其他区域也用到的通用词（此处归 bucketcfg）----
  Save: "保存",
};

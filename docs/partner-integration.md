# 内容合作方接入契约

状态：文本同步已有未交付的候选实现，其余为适配设计；不代表合作方 API 已接入或真实发布已验收。
本文件细化冻结产品规格，不修改 `product-plan-v1.md`。网页 AI 测量优先推进，
合作方接入不成为任意话题测量的前置条件；发布成功也不等于被 AI 收录或引用。
实际交付与缺口分别记入 `WORKLOG.md`、`TODO.md`。

## 1. 产品与复用边界

合作方将获授权的企业资料同步至现有知识库；GEO 基于同一知识版本生成正文及
渠道变体，再通过合作方的已验证发布能力交付。合作方不获得整租户知识读取权。
AI 工作台发起导入、生成、分发和查询，详情页查看同一来源、正文和发布账本；
不建立合作方专属内容库、编辑器、审批队列或第二套发送状态机。

已有接缝与新增范围：

| 范围 | 当前可复用接缝 | 本设计待实现 |
|---|---|---|
| 知识 | `crates/api/src/knowledge/mod.rs` 的上传、imports、source versions、解析任务和用途变更 | 服务身份、外部来源映射、增量游标与撤销桥 |
| 内容 | 已保存的 ContentRevision、ChannelVariant、版本级导出及媒体绑定 | 精确版本的合作方交付清单、能力映射 |
| 发布 | 单篇分发请求、PublicationIntent、outbox、发送尝试及追加查回证据 | 合作方 API transport、回调 Inbox、轮询及状态映射 |
| 展示 | P00、知识/内容详情、发布记录及报告 | 同资源投影及伙伴交付证据，不另起数据孤岛 |

以上是复用边界，不意味着现有 Cookie API 已支持合作方 API Key；
外部增量文件替换、生命周期完整性与真实富文本发布仍须实现并单独验收。

## 2. 连接、身份与最少新增记录

- `PartnerConnection` 由有权限的管理员建立，绑定服务端确定的 operator、tenant、
  项目允许集、外部企业映射、渠道/账号允许集、能力版本及状态。
  首包每个连接只绑定一个项目，跨项目共享另走现有知识授权，不靠相同外部 ID 推断。
- 外部 ID、客户名称、凭据、地址和原始资料仅存受控业务存储，不进入公开仓库或日志。
- 权限拆分为 `knowledge:write`、`content:export`、`publication:submit`、
  `publication:read`；回调使用独立验签凭据，不能当作导入或导出密钥。
- 新增映射/游标记录绑定连接与完整作用域：外部来源 ID → 现有 Source，
  外部版本 → SourceVersion/ImportJob；外部发布 ID → 原 PublicationIntent/Attempt。
  唯一键含作用域和 connection_id，不用全局外部 ID 或正文摘要跨客户去重。
- 加密保存需要出站使用的秘密；入站 API Key 只保存不可逆验证值及 key_id。
  支持可选到期、轮换、撤销和操作审计；每次访问复核连接权限。管理员签发或轮换时
  可一次性查看并复制密钥；仅在当前组件暂存，关闭或切换身份／项目后清除。
  密钥不进入聊天、浏览器持久存储、查询缓存、日志或服务端幂等响应存储。

## 3. 增量知识导入与撤销

首包采用合作方向 GEO 推送，避免先依赖未确认的合作方拉取 API。
每项包含 `external_source_id`、`external_version`、单调 `source_sequence`、
`operation=upsert|revoke`、用途、业务生效时间、内容摘要和文本/已核验上传对象引用。
原始外部版本作为不透明字符串保存，不按字符串大小推断新旧；来源定位保留版本溯源。

1. 稳定 `Idempotency-Key` 绑定规范化请求摘要；同键同输入复用 operation，
   同键异输入返回 409。外部来源/版本复用但字节或语义不同，同样冲突。
2. 服务端校验对象归属、真实字节与摘要，拒绝任意磁盘路径和下载地址；
   大文件复用有界上传链路。首包按实际解析能力接收文本/文件，不承诺网页抓取或 OCR。
3. 同事务写入映射、SourceVersion、ImportJob 和 outbox；逐项返回
   `accepted|unchanged|rejected` 与稳定资源 ID。accepted 不表示解析成功。
4. 每来源按 sequence 串行推进接收水位，旧版本只返回已知结果，不回滚当前指针。
   首包 upsert 是完整来源快照，不接受缺前序的字段补丁；解析完成时再次比较版本，
   迟到旧任务不能覆盖新版本。重试从原任务失败单元恢复，不重复收费。
5. 解析/索引失败保留原可用 release；新版本仅在可用后切换，partial 明示覆盖。
   授权用途收紧不能等解析完成：internal/revoke 受理事务立即退出公开检索与待发送检查。
6. revoke 保存不可复活的序号墓碑和影响关系，旧重放不能恢复资料；恢复需更高序号、
   明确 upsert 及当前权限。历史原件/版本/证据按保留策略保存，不伪称外部文章已撤回。
7. 已公开资产进入现有更新/撤回义务；合作方不支持该动作时明确 capability_missing。
   停止同步只停止拉取/推送处理，不等于来源撤销；连接撤销立即禁止新的导出和发送。

批次保存逐项持久结果与分页游标，成功项不因其他项失败回滚。
合作方只能在结果已持久化后推进已确认水位；断线继续读原 operation，不重新生成来源。

## 4. 正文/变体交付与发送

- 导出精确 `content_asset_id + content_revision_id + variant_id`，不能取发送时的 latest。
  交付清单包含 schema_version、标题、格式、规范化载荷摘要、语言、媒体对象版本/摘要、
  证据引用 ID 和授权目标引用；不默认导出企业原文、内部资料或其他账号身份。
- JSON/HTML/Markdown/图片按已验证能力选择；富文本沿用版本化结构与媒体清单。
  不支持节点/图片时阻断该分支，不静默扁平化。下载采用作用域鉴权和短时、受限授权，
  导出本身不记作发布，导出撤销无法追回已经下载的字节。
- GEO 主动提交为首包模式；不得在合作方拉取导出后另做一套无账本自动发送。
  同一 ContentDistributionRequest/覆盖项创建或复用既有 PublicationIntent；
  伙伴映射不改变业务去重身份，不因换请求、连接密钥或周期重复发布。
- 发送前重新核验来源/媒体用途、内容检查、账号/渠道授权、预算及能力版本。
  持久化 attempt 与发送授权后，才传输正文/媒体给合作方；沿用
  `rich-publication-delivery.md` 的授权线性化语义，不跨网络等待持有数据库事务。
- 发送冻结载荷、稳定外部幂等键与关联 ID。合作方必须提供同键去重或原请求查回能力；
  两者均未证实则不启用无人值守自动发布。导出仍可独立提供，但不算发布闭环。
- 超时/回执丢失/进程重启进入 unknown，只读查回，不新建意图、换账号或盲目再发。
  只有权威证明未发送且策略允许，才在原逻辑发布下受控再尝试；预算未知保留预留。

## 5. 回执、公开验证与恢复

回执绑定原连接、请求/意图/尝试、外部任务/资产 ID、冻结载荷摘要、渠道目标、
事件 ID、外部状态/版本、发生时间和接收时间。回调与轮询进入同一追加证据验证器；
先持久 Inbox 再应答，去重与投影同事务，迟到/乱序事件不覆盖原尝试或历史报告。
外部序号缺失或状态互相矛盾时保留证据并查回，不能以接收时间决定成功。

| 合作方事实 | 本地含义 |
|---|---|
| 请求已受理、排队、处理中 | 已提交/平台处理中，不计已发布 |
| 可信回执证明原请求生成了指定资产 | 已发布，等待独立公开验证 |
| 匿名读回与原发送关联、作者、全文及所需媒体/结构一致 | 已验证 |
| 明确拒绝且证明未发生发布 | 明确失败；是否重试另受策略约束 |
| 超时、普通 404、无法关联的文章、未知新状态 | unknown；资产存在只作观察 |
| 取消请求已受理 | 仍须查回实际结果，不假定撤销已发送动作 |

轮询按原外部任务/幂等键，持久租约、退避和游标分页恢复；暂停项目仍核对在途结果。
公开 URL 只作线索，验证器限制协议、目标、DNS/重定向和响应大小，防止 SSRF；
公开读回不带伙伴密钥或账号 Cookie。费用凭交易/用量证据去重，不凭 published 字符串结算。

## 6. 拟议 GEO 接口与安全验收

下列为 GEO 接口契约，不是合作方 API 声明。前两项已有候选实现，尚待组合 CI 与部署；其余路径待实现：

| 路径 | 职责 |
|---|---|
| `POST /api/v1/partner/connections/{id}/knowledge-syncs` | 幂等受理增量批次；202 + operation_id |
| `GET /api/v1/partner/connections/{id}/knowledge-syncs/{operation_id}` | 分页读回逐项资源、版本、解析状态 |
| `GET /api/v1/partner/connections/{id}/content-exports/{revision_id}` | 鉴权导出精确版本；不会发送 |
| `POST /api/v1/partner/connections/{id}/publication-requests` | 复用既有分发服务与原意图 |
| `GET /api/v1/partner/connections/{id}/publication-requests/{id}` | 读取请求、回执和验证，不触发重发 |
| `POST /api/v1/partner/connections/{id}/publication-events` | 验签并持久接受回调 |

路径 connection_id 必须与服务身份绑定，正文不得自报 tenant/operator 提权。
回调采用维护中的标准签名实现，签名覆盖原始字节、时间戳、事件 ID、目标路径与
connection_id；有限时钟窗口、key_id 轮换及 `(connection_id,event_id)` 持久去重。
同事件异载荷冲突，跨连接重放拒绝；过期事件需重新签名投递或由鉴权轮询恢复，
不能放宽窗口绕过验签。限流、载荷上限、稳定错误码和脱敏审计同样必需。

首包验收：重复/乱序导入、撤销后旧重放、解析崩溃、跨租户资源/回调拒绝、
发送前后崩溃、回调早于 HTTP 回执、超时查回只发一次、密钥轮换、富文本能力不足、
公开资产关联、迟到证据不改历史报告；最后以授权真实资料和渠道单独验收。
最低待确认信息：伙伴鉴权/测试环境与限额、稳定企业/资料/版本标识及顺序语义、
渠道/账号/格式能力、发布幂等与查回/回调协议、公开链接及更新/撤回能力。
这些信息决定 transport 映射与真实验收，不阻塞上述内部契约和网页测量开发。

## 7. 文本同步候选实现

实现入口为 `crates/domain/src/partners/partner.rs`、`crates/api/src/partners/partner.rs`、
`crates/persistence/src/partners/mod.rs` 和迁移 `0061_partner_knowledge_sync.sql`。
目前仅 PostgreSQL 模式装配；管理端连接创建、密钥签发和轮换的 HTTP/UI 候选入口见下节。
上线与实际接入尚未验收，不能直接作为已可自助接入的功能交付。

请求使用 `Authorization: Bearer <key_id>.<secret>`，推送另带
`Idempotency-Key` 和 `Content-Type: application/json`；凭据不经聊天传递。
服务身份绑定一个项目，不接受调用方传入 tenant/operator/project 改变作用域。
请求结构如下，尖括号内容由调用方实际输入计算或替换，不是可直接提交的测试数据：

```json
{
  "schema_version": 1,
  "items": [
    {
      "external_source_id": "<来源标识>",
      "external_version": "<不透明版本标识>",
      "source_sequence": 1,
      "effective_at": "<RFC3339 时间>",
      "action": {
        "operation": "upsert",
        "name": "<资料名称>",
        "purpose": "public",
        "media_type": "text/plain",
        "text": "<完整文本>",
        "content_sha256": "<文本 UTF-8 字节的 SHA-256 小写十六进制>"
      }
    }
  ]
}
```

撤销使用同样的外层来源、版本、序号及时间字段，`action` 为
`{"operation":"revoke"}`。操作标签嵌套于 `action`，不是顶层字段。
首包仅接收 UTF-8 TXT/Markdown 完整快照，不接收下载 URL 或本地路径。
公开与内部用途分别为 `public`、`internal`。

202 返回持久 operation 回执，不代表所有项已经完成。调用方用原 operation 查询
逐项结果，分页参数为 `after_index`、`limit`；失败恢复不新建来源或丢弃前面的成功项。
重试使用原幂等键及原输入，结果中的资源 ID 指向现有企业知识库。
接入验收须覆盖受理后立即收紧公开用途、后台停机恢复、序号乱序、旧版重放、
密钥撤销及跨项目拒绝；候选代码存在不能替代这些实库与上线验证。

## 8. 项目设置中的连接管理

工作区管理员在项目设置的“合作方连接”中创建连接、签发／轮换／撤销密钥。
管理接口使用第一方会话、项目权限及 CSRF 校验，不接受合作方 Bearer 代替管理员身份。
基础路径为 `/api/v1/projects/{project_id}/partner-connections`：

- `GET`／`POST` 集合：分页元数据／创建项目连接，创建正文为 `{}`。
- `GET`／`POST /{connection_id}/keys`：分页密钥元数据／一次性签发，
  签发正文为 `{"expires_at":null}` 或未来 RFC3339 时间；没有强制到期。
- `POST /{connection_id}/keys/{key_id}/rotate`：同签发正文，事务内使旧密钥失效。
- `POST /{connection_id}/keys/{key_id}/revoke` 和
  `POST /{connection_id}/revoke`：正文 `{}`，撤销成功返回 204。

分页采用 `after` UUID 游标及 `limit`，返回 `{items,next_cursor}`；
第一方客户端附带的项目／租户选择器须与服务端授权作用域一致。
签发／轮换返回 `{key,bearer}`，仅当次显示；其余响应不包含密钥或验证摘要。
管理命令不进入通用持久幂等响应缓存，客户端不自动重试签发／轮换。
响应丢失时刷新元数据并显示结果尚未确认，不凭不完整历史列表猜测新密钥，
不强迫撤销已有工作凭据。用户可明确选择重新签发或撤销不再需要的密钥。
连接撤销不等于来源撤销，也不代表外部已发布文章已下线。

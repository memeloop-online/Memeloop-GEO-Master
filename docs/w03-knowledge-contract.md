# W03 企业知识库实施合同

状态：实施设计  
依赖：冻结规格 `product-plan-v1.md`、W02 原子启动合同  
边界：本文件细化 W03，不修改或替代冻结产品规格。

## 1. 交付目标

W03 建立第一层 fan-out 的可信输入：

```text
文件 / URL / 文本 / 已有对象 / 已有知识集合
  → 原件与不可变来源版本
  → 可定位片段、产品与事实
  → 不可变 KnowledgeRelease
  → 有限、版本化的文档覆盖清单
```

W03 不生成正文、不展开平台目标、不发布、不测量，也不增加人工审批。

## 2. 不可混用的对象

- `ProjectSettings.initial_sources` 是启动配置输入，不是已处理知识。
- `UploadSession` 是字节传输状态，不是可用资料。
- `Source` 是可变的资料身份和当前用途。
- `SourceVersion` 是不可变原件/快照版本。
- `ImportJob` 保存解析阶段、逐单元结果和恢复点。
- `KnowledgeRelease` 是一组已完成来源版本、事实修订和索引的不可变快照。
- `DocumentManifest` 是有限覆盖规划，不是已生成内容。

客户端提供的 `version_ref`、`content_hash`、对象路径或 URL 都不能替代服务端核验。

## 3. 领域实体

所有实体和唯一键都携带 `operator_id`、`tenant_id`、`project_id`。

### 3.1 上传与原件

`UploadSession`

- `upload_session_id`
- `revision`
- `filename`
- `declared_media_type`
- `expected_size`
- `expected_sha256`
- `purpose`: `public | internal`
- `state`: `created | uploading | uploaded | committed | failed | expired | cancelled`
- `expires_at`
- `staging_object_ref`
- `committed_object_id`
- `operation_id`

`StoredObject`

- `object_id`
- `object_version`
- `backend`
- `opaque_key`
- `actual_size`
- `detected_media_type`
- `sha256`
- `state`
- `created_at`

对象 key 只能由服务端生成。API 不接受磁盘路径、bucket key 或任意下载 URL。

### 3.2 来源、解析与知识

`Source`

- `source_id`
- `revision`
- `kind`: `file | url | text | object | knowledge_collection | manual`
- `name`
- `purpose`: `public | internal`
- `state`: `active | removed`
- `locator`
- `current_version_id`
- `sync_enabled`
- `next_sync_at`
- `last_sync_at`

`SourceVersion`

- `source_version_id`
- `source_id`
- `version`
- `object_id` / `object_version`
- `content_sha256`
- `captured_at`
- `original_url`
- `parent_version_id`
- `parser_version`
- `extraction_version`
- `created_at`

`ImportJob`

- `import_job_id`
- `operation_id`
- `source_id`
- `source_version_id`
- `stage`: `acquire | parse | extract | index | release`
- `status`: `queued | running | partial | succeeded | failed | cancelled`
- `attempt`
- `lease_until`
- `input_hash`
- `stage_output_refs`
- `completed_units`
- `failed_units`
- `errors`
- `resumed_from`

`Chunk`

- `chunk_id`
- `source_version_id`
- `ordinal`
- `kind`: `paragraph | table | image_description`
- `text`
- `text_hash`
- `locator`
- `product_ids`
- `market`
- `language`
- `extraction_method`
- `confidence`

`Product`

- `product_id`
- `revision`
- `name`
- `model`
- `aliases`
- `state`
- `evidence_refs`

`Fact`

- `fact_id`
- `revision`
- `subject_id`
- `product_id`
- `model`
- `attribute`
- `typed_value`
- `unit`
- `market`
- `language`
- `currency`
- `effective_from`
- `effective_to`
- `status`
- `pinned`
- `evidence_refs`
- `supersedes_fact_id`

`KnowledgeRelease`

- `knowledge_release_id`
- `sequence`
- `previous_release_id`
- `source_version_refs`
- `fact_revision_refs`
- `index_build_id`
- `pipeline_versions`
- `content_hash`
- `coverage`
- `created_at`

`KnowledgeUsage`

- `knowledge_usage_id`
- `knowledge_release_id`
- `source_version_id`
- `chunk_id`
- `fact_id`
- `fact_revision`
- `consumer_kind`
- `consumer_id`
- `consumer_revision`
- `purpose`

### 3.3 证据定位

定位器为带 `kind` 的结构化值：

- PDF：`page`，可选 `bbox`、坐标系、OCR 标记。
- DOCX：`heading_path`、`paragraph_index`，可选表格位置。
- XLSX：`sheet`、`range`、`header_range`。
- 网页：`snapshot_object_id`、`original_url`、可选 selector 和字符区间。
- TXT/Markdown：行号和字符区间。
- CSV：行列范围和表头。
- 手工事实：绑定 `manual` 来源的不可变文字版本。

提取事实必须引用实际存在的 chunk 和 SourceVersion。不存在的模型引用不能入库。

## 4. 上传与导入

### 4.1 文件上传

1. `POST /api/v1/knowledge/upload-sessions`
2. `PUT /api/v1/knowledge/upload-sessions/:id/content`
3. `POST /api/v1/knowledge/upload-sessions/:id/complete`

创建会话返回文件限制、过期时间和上传目标。字节上传不经过 JSON 幂等中间件。完成操作核验实际大小、SHA-256、类型和对象归属；核验成功后在同一事务创建 Source、SourceVersion、ImportJob、Operation 和 outbox。

首版允许整文件重传，不要求分片上传。默认单文件上限 100 MB、单批次 100 个文件。

### 4.2 URL、文本、对象和集合

`POST /api/v1/knowledge/imports`

批次请求使用 `client_item_id`，逐项返回接受或错误。一项失败不回滚其他成功项。

- URL 保存快照后才形成不可变 SourceVersion。
- URL 每次 DNS 与重定向都必须阻止本机、私网和元数据地址。
- 文本超过接口字节限制时必须转文件上传。
- 对象引用只能引用当前授权范围内已提交对象。
- 集合引用必须冻结到明确 release；内部资料不能被升格为公开资料。
- W02 的 `initial_sources` 只允许幂等 materialize 一次。

## 5. 解析、抽取与索引适配器

领域层只依赖接口：

```text
ObjectStore
DocumentParser
OcrAdapter
FactExtractor
KnowledgeIndexer
KnowledgeAnswerer
```

Tika、Tesseract、pgvector、LLM 和外部抓取器是可替换适配器，不成为领域模型的一部分。

阶段输出和逐页/工作表结果必须持久化。重试只处理缺失或失败单元。扫描识别、向量或问答能力未配置时返回 `capability_missing`，不能用空结果冒充成功。

索引失败不切换当前 KnowledgeRelease。部分解析可以形成带覆盖说明的 release；未成功来源不删除旧可用版本。

## 6. 事实冲突与用途

事实比较键至少包含主体/型号、属性、市场、币种和适用时间。

选择顺序：

1. 仍有效且有授权证据的客户固定值；
2. 范围和生效时间明确的来源；
3. 无法判定时保留候选并标记 `conflicted`。

抓取时间更新不能覆盖业务生效时间更明确的价格表。

来源改为 `internal` 或被移除后：

- 立即退出新的公开检索和发布前检查；
- 已排队公开任务必须重新检查；
- 已公开资产只登记更新/撤回义务，不宣称已从外部删除；
- 无关文档和平台分支继续。

## 7. API

- `GET /knowledge/capabilities`
- `POST /knowledge/upload-sessions`
- `PUT /knowledge/upload-sessions/:id/content`
- `POST /knowledge/upload-sessions/:id/complete`
- `POST /knowledge/imports`
- `GET /knowledge/sources`
- `GET /knowledge/sources/:id`
- `GET /knowledge/sources/:id/versions/:version_id`
- `POST /knowledge/sources/:id/versions`
- `PATCH /knowledge/sources/:id`
- `DELETE /knowledge/sources/:id`
- `POST /knowledge/import-jobs/:id/retry`
- `GET /knowledge/products`
- `GET /knowledge/facts`
- `POST /knowledge/facts`
- `PATCH /knowledge/facts/:id`
- `POST /knowledge/search`
- `POST /knowledge/ask`
- `GET /knowledge/releases/current`
- `GET /knowledge/releases/:id`
- `GET /knowledge/sources/:id/impact`

统一要求：

- 会话推导 operator/tenant；`project_id` 只是资源选择器。
- 更新使用 `If-Match`。
- 输入拒绝未知字段。
- 异步接受返回 `202` 和稳定 Operation。
- 同幂等键异请求返回冲突。
- 跨租户 ID 返回 404。
- 错误补充 `field`、`retryable` 和稳定 `details.reason`。
- 语义缺失与冲突是可返回的业务结果，不一律变成 500。

## 8. 事务与事件

事务边界：

1. 导入接受：业务幂等 + Source/Version + Job + Operation + `knowledge.import.accepted`。
2. 阶段完成：输出引用 + 单元结果 + Job 进度 + 下一阶段 outbox。
3. Release 切换：插入不可变 release、明细、更新项目当前指针、`knowledge.release.created`。
4. 用途/移除：更新 Source revision、即时授权状态、影响记录和 outbox。

主要事件：

- `knowledge.import.accepted`
- `knowledge.source.version.ready`
- `knowledge.import.partial`
- `knowledge.import.failed`
- `knowledge.release.created`
- `knowledge.source.policy_changed`
- `knowledge.source.removed`
- `knowledge.usage.invalidated`
- `document.manifest.frozen`

事件不携带全文、模型密钥或签名 URL。

## 9. KnowledgeRelease 到 DocumentManifest

规划器输入：

```text
config_revision_id
+ knowledge_release_id
+ document_scope
+ planner_version
```

候选维度：

```text
有效产品 × 市场 × 语言 × 有限内容类型 × 已明确问题簇
```

`doc_key` 从规范化业务维度确定生成。每项保存精确来源依赖和 `dependency_hash`，状态为：

- `planned`
- `blocked`
- `deferred`
- `not_applicable`

W03 不使用 `ready` 表示正文已生成。缺失、冲突、内部资料和预算延后都保留清单项及原因。

首次规划封存 W02 创建的 DocumentManifest 骨架；后续知识更新创建 manifest 新修订。DistributionManifest 保持 `awaiting_documents`。

只有依赖哈希变化的项失效。不能仅因 KnowledgeRelease ID 变化而重建全部文档。

## 10. P03—P05

P03：

- 来源、产品/事实、问答/案例、偏好、变更记录标签；
- 来源表显示名称、类型、用途、产品、处理状态、片段数、事实数和同步时间；
- 导入文件、文本、多 URL、对象和集合；
- 批量导入逐项状态与只重试失败项。

P04：

- 左侧原文/快照，右侧片段与事实；
- 点击事实定位原文，点击片段查看事实与影响；
- 显示来源版本、部分失败覆盖、用途和影响链。

P05：

- 请求绑定明确 KnowledgeRelease；
- 返回答案状态、证据和缺口；
- LLM 未配置时显示“证据摘录模式”；
- 无依据时显示“当前资料没有这项信息”；
- W05/W06 未接入前，“生成文章”禁用并说明原因。

## 11. 完成定义

- P01 真实文件上传，不显示假完成。
- P03/P04/P05 使用真实接口和持久化数据。
- 原件、版本、片段、事实和 release 可以互相追溯。
- 内部资料不进入公开检索。
- 重复导入不重复处理或计费。
- 单页/单来源失败不阻塞无关分支。
- KnowledgeRelease 能封存有限 DocumentManifest，分母不因失败缩小。
- PostgreSQL 空库迁移、跨租户负例、重启恢复和浏览器路径有测试证据。
- 未配置的 OCR、向量、问答和抓取能力明确列为未完成。

## 12. 明确不在 W03

- Campaign、Opportunity、ContentBrief 策略引擎；
- 正文与渠道变体生成；
- 平台覆盖矩阵、账号/出口调度与发布；
- AI 基线测量；
- reduce、周报和完整商业账务；
- 人工审核或放行队列；
- 无限网页爬取；
- 第二套无版本主文档正文表。

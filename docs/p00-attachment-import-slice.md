# P00 附件入库实施接缝

依据 `product-plan-v1.md` 0.3、0.9、0.12；这是下一开发纵切的实施说明，不改变冻结规格。

## 当前状态

上传、核验、对象引用、回合绑定、Object 导入和原生导入/检索工具循环已实现。
`TurnInput` 携带真实 message ID 与附件引用，executor 从持久消息重建输入；
内存及 PostgreSQL 按稳定收据键重放导入。新增实库分支等待本次 CI 复验。
上传成功仍不代表已入库；只有导入收据和检索证据证明对应步骤完成。

## 已实现纵切及边界

持久消息附件 → 模型选择显式导入工具 → Rust 解析已提交对象 →
形成 SourceVersion/KnowledgeRelease → 模型调用知识检索 → 引用证据回答。

不增加人工确认步骤，不另写 Agent 循环，不把原始文件塞进 prompt。
首个切片使用确定性 TXT/Markdown 解析器；后续 CSV 契约见 `csv-import.md`。可配置的 PDF 文字解析现已复用同一显式附件导入入口，返回后台排队收据，不立即声称已有知识版本；逐页状态、失败页重试及验证边界见 `pdf-import.md` 和工作日志。其余未接入格式逐项返回能力缺失。

### 1. 持久回合输入

`crates/domain/src/agent.rs` 的 `TurnInput` 增加真实 `message_id` 与
`attachments: Vec<AttachmentReference>`。`run_executor` 从 repository 已受理消息构造，
不从 HTTP 原始请求重新取值。serialized input hash 自然包含附件版本。
重入时从 Run → Turn.root_message_id → Message 重建相同输入。

### 2. 对象导入

领域及 PostgreSQL repository 的 `import_batch` 增加 Object 分支：

- 按 operator/tenant/project 查询已提交对象及原字节，验证版本和摘要。
- 支持的文本格式校验 UTF-8；保留对象 ID/版本与原始摘要到 SourceVersion。
- 复用确定性解析和 release 逻辑，不伪装 inline text 丢失原件溯源。
- PostgreSQL `import_text_in_transaction` 需区分新建对象与引用已有对象，避免重复 INSERT 主键。
- 内存解析需区分 inline 文本限制与已上传对象，不以 inline 限制拒绝合法上传。
- 使用稳定 `agent-attachment:{attachment_id}` 作为项目内 client_item_id；
  同输入重放同收据，改变用途返回冲突，不静默改写历史用途。
- 并发同项重试须在业务事务中串行判定或冲突后读取已有收据，不引入运行时并发门槛。

### 3. Rust 工具及作用域

新增 `knowledge.import_attachments.v1`，输入仅为：

```json
{"items":[{"attachment_id":"uuid","purpose":"internal"}]}
```

作用域、对象版本、文件名及业务幂等键由 Rust 补齐，模型不能指定。
每次 run 的不可变附件绑定只允许该回合已受理的附件，不能放在共享可变
`RepositoryHostOps` 字段中。调用前重验作用域和已提交元数据。
每项返回 status、source_id、source_version_id、knowledge_release_id 或具体错误；
不返回整份原始字节，不以一项失败覆盖其他成功项。

现有 host surface 是 `geo.hostops.v2`；修改时保留已存在的版本化工具契约。

### 4. MemeLoop 接入

在原生 registry 注册 `knowledge_import_attachments`，动态工具 schema 提供当前附件 ID、
文件名和 MIME，模型按用户入库意图调用。保留 `knowledge_search`，用导入返回的 release 检索。
允许仅附件消息；使用真实 message ID，不继续以 turn ID 代替。

上游 canonical 附件仅含 contentHash/filename/mimeType/size，且模型请求会强制通过
`readAttachmentData` 装入原始字节。本适配在 canonical message metadata 与动态工具 schema
中传递附件元数据，权威对象绑定留在 Rust，不提供整份文件读取器。
上游还要求 canonical user turn ID 等于 message ID：适配使用真实 message ID，
GEO 独立 turn ID 留在运行与完成记录中。通用不透明附件/外部 turn 映射可向上游提出，
不复制聊天框架。

## 必须验收

- 上传不创建知识来源；导入后能以原对象版本定位来源。
- 真实 V8 MemeLoop 执行 import → search → assistant，并返回可核对引用。
- 重复和并发调用不多建 source/version/release；用途变化不覆盖旧收据。
- 跨项目、未在本回合绑定的附件和元数据不匹配被拒绝。
- 有效 TXT 与不支持文件同批时保留逐项结果。
- 从持久消息重建输入后，重复业务工具调用复用原结果。

这些只证明业务导入重放安全，不证明完整 Agent 中途 checkpoint 恢复或工具 ledger 已完成。

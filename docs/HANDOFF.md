# Memeloop GEO 工程交接说明

更新时间：2026-10-04

适用分支：`integration/w00-pr-stack`（本地集成，尚未合并到 `main`）

当前实现基线：P00 工具循环、周期冻结规划与内容第一层 fan-out、持久内容扫描、第二层分发覆盖账本、账号双来源、渠道账本、周报与周期推进；精确验证进度见 `WORKLOG.md`。分发 outbox 已接渠道任务及生成版本执行路径，真实外部发送尚未验收；不配置模型时 AI 保持未开启，权益、费用和真实部署验收尚未完成。

本文是脱离历史对话后的工程入口。接手者不需要读取 Codex、聊天记录或本地代理上下文；产品范围、当前状态、未完成任务和验证方式均以仓库内容为准。

最新绿色基线：`e8ba567`，Actions `37170397496` 全部通过。迁移 `0018` 将正式分发 outbox 原子物化为独立渠道目标，与原有测量计划共存；发送路径重新读取冻结正文与全部引用，声明尝试时关联分发意图，可信公开读回后保存证据。正式报告已采用覆盖矩阵分母，但尚未把该回执证据投影到目标报告，因此不能据此宣称完整发布报告闭环。默认连接器能力仍未完成实测配置，真实外部发送与搜索验收缺失。精确测试状态见工作日志；下文早期“outbox 尚未接发送”描述以本段接线状态和这些限制为准。

## 1. 文档的权威顺序

1. [`product-plan-v1.md`](product-plan-v1.md)：产品、架构、页面、领域模型、API、容量和验收的完整实施基线。当前版本为 1.1。除非产品负责人再次明确变更需求，不要直接修改。
2. [`TODO.md`](TODO.md)：只保留未完成工作和当前验收目标。完成后从这里删除，并将结果写入工作日志。
3. [`WORKLOG.md`](WORKLOG.md)：已完成工作、验证证据、已知限制和重要技术判断。
4. 本文：仓库导航、运行方式、实现现状和接手步骤。
5. [`w03-knowledge-contract.md`](w03-knowledge-contract.md)：W03 企业知识导入纵切的细化契约；若与产品规格冲突，以产品规格为准。

不要把进度、临时判断或日常日志补进产品规格书；分别维护 `TODO.md` 与 `WORKLOG.md`。

## 2. 产品主线

系统的核心不是单个平台发布器，而是可恢复的两次 fan-out 和一次 reduce：

1. 企业资料经过解析、溯源和版本化，fan-out 为有限、可封存的知识文档清单。
2. 每份知识文档按目标平台、账号、连接器和网络策略 fan-out 为独立发布分支。
3. 发布证据、站点验证和独立 AI 渠道测量进入 reduce，形成不可变周报和下一轮增量。

P00 AI 工作台是默认入口。用户应能通过对话或附件调用所有产品能力；P01–P16 是需要查看或编辑具体细节时的专业界面。MemeLoop 负责 Agent/Loop 编排，Rust 是权限、预算、状态、清单、发布、测量、费用和外部副作用的权威边界。

## 3. 当前已经实现

### 工程与身份

- Rust/Axum 后端、TypeScript/React/Fluent 前端、pnpm/Cargo 工作区及 CI 基线。
- 同源 Cookie 会话、CSRF/Origin 校验、Host 到 Operator 解析、Membership 到 Tenant 的服务端授权。
- 本地内存开发模式和 PostgreSQL 权威模式；配置了 `DATABASE_URL` 后，连接或迁移失败会直接终止，不会静默回退到内存。
- 项目创建、配置修订、三类独立估算、原子启动、稳定幂等 Operation、Cycle、Workflow 和两份未封存 manifest 骨架。

### 企业知识库 W03-A

- 上传会话、SHA-256 字节核验、批量导入、来源/版本、确定性文本分段、不可变 KnowledgeRelease、证据定位和 evidence-only 问答。
- P01 已能物化文本、URL 引用和已上传对象；P03–P05 已接真实 API。
- 当前只确定性解析 `text/plain` 与 `text/markdown`。PDF、DOCX、XLSX、CSV、网页抓取、OCR、向量检索、LLM 回答和结构化事实提取尚未实现。

### P00 AI 工作台

- P00 是侧边栏首项，项目根、项目切换和首次启动完成后默认进入 `/chat`。
- 局部集成 `@memeloop/react-ui` 的 `AgentChatView`，外围应用壳继续使用 Fluent UI。
- 已定义并实现内存版 Conversation、Message、Turn、Run、AttachmentReference、RuntimeCapability 和递增 ConversationEvent。
- 已有会话创建/列表/详情、消息提交、Turn 取消和 SSE 重放 API；支持幂等提交、同键异请求冲突、附件-only 消息、跨项目隔离、`after`/`Last-Event-ID` 恢复。
- P00 已接多附件选择/拖拽/粘贴、逐项上传及失败重试；专用附件 API 核验字节与摘要，提交消息时核对作用域和已提交对象元数据。TXT/Markdown 可经模型显式导入工具形成知识版本，再检索并引用回答；上传本身不直接入库，原始大文件不塞入模型上下文。
- 真实 MemeLoop 已执行模型 → `knowledge_search` → Rust 检索 → 模型回答；JSON function-tool 协议可用，尚无流式工具分片、多回合历史或中途恢复。
- Rust JS Runtime 未接入时明确返回 `capability_missing`，界面不会伪造 AI 回复。

相关入口：

- 前端：`apps/web/src/pages/AgentWorkbenchPage.tsx`
- 前端 API：`apps/web/src/api/agent.ts`
- Agent 领域：`crates/domain/src/agent.rs`
- Agent HTTP/SSE：`crates/api/src/agent.rs`
- 内存/持久化边界：`crates/persistence/src/agent.rs`
- 应用装配：`crates/api/src/lib.rs`、`crates/app/src/main.rs`

### W00 兼容探针（引擎腿）

- 新增隔离 crate `crates/worker`（包名 `geo-worker`），基于 `deno_core 0.412.0` 内嵌 V8；`geo-api` 已接入运行时边界，应用在显式本地模型配置下装配真实 bundle，否则为 `unconfigured()`。
- 7 项原始探针覆盖 ESM 跨模块加载、Promise 与顶层 await、host op、墙钟超时、外部取消、堆上限可恢复终止和 checkpoint 序列化。原 PR 验证记录见工作日志；当前集成修改必须单独复验，不能沿用旧测试结果。
- 隔离边界：JS 只能经 4 个窄 host op 触达 Rust；模块仅限 Rust 注入的内存 allow-list，无文件系统、网络或包 registry 解析；checkpoint 是 Rust 拥有的宿主状态序列化，不是 V8 堆快照。
- 回退路径不需要第二套运行时：`deno_core` 自带 `quickjs` feature，可在同一 API 上切换引擎。

相关入口：

- 探针运行时：`crates/worker/src/runtime.rs`
- 内存模块加载器：`crates/worker/src/loader.rs`
- 窄 host op：`crates/worker/src/ops.rs`
- 最小 bundle：`crates/worker/src/bundle.rs`
- 探针测试：`crates/worker/tests/probe.rs`

### W00 安全 Host Ops

- 封闭且带版本的 op 面（`geo.hostops.v5`）：保留原有八项及四项 `channel.*.v1`；追加内容启动/执行读取、分页项读取、准备/生成/检查和交接七项 `content.*.v1`。附件导入只接受 Rust 已绑定到当前回合的对象；业务工具只传受限资源引用，JS 无法取得 session、代理凭据、SQL、任意网络、文件、进程或环境变量。
- 边界方向为 `geo-api → geo-worker`，worker 从不反向依赖 API。请求 DTO 全部 `#[serde(deny_unknown_fields)]` 且不携带 tenant/project 选择器，作用域只能来自 Rust 侧 bridge。预算、单次调用截止与取消统一在 `HostBridge::invoke` 施加。
- `RepositoryHostOps` 已实现知识检索/附件导入、文档清单读取、报告及渠道工具和可注入的模型调用；内容工具另由受限 Rust 服务执行。本地开发模型装配见第 5 节。清单读取保留规划状态、阻断原因及覆盖分母，不把规划项 ID 冒充正文版本；正式文档×平台展开、真实发布与测量验收仍缺失。

### W00 Run Executor

- `append_message` 提交**之后**由 handler 调用 `run_executor::dispatch`：`begin_run`（原子 `UPDATE … WHERE status='queued' RETURNING`）→ `run_turn` → `finish_run`（**单事务**写 run 状态、错误、assistant 消息、turn 终态与事件）。HTTP 响应只陈述**受理**，永不乐观地写成 `running`。
- **运行时 flavor 是本模块的硬约束，改任何一处都会静默出错**：`deno_core` 的 op driver 用 `deno_unsync::tokio::spawn` 派生首次轮询未完成的 op future，该函数断言 `runtime_flavor() == CurrentThread` 并据此把非 `Send` future 伪装为 `Send`。因此**隔离体必须在自己的 current-thread 运行时上驱动**（且在同一个 `spawn_blocking` 任务内构建与销毁，因为隔离体非 `Send`），而**能力调用必须投递回应用运行时**（tokio I/O 资源绑定创建它的运行时，连接池不能跨 turn 迁移）。在多线程运行时上驱动隔离体在 debug 下中止进程、在 release 下是未定义行为。理由写在 `HostBridge::new` 的文档注释里。
- 未配置装配下提交消息会产生能力缺失；配置后真实 bundle 单回合已由应用装配测试覆盖，测试使用注入 transport，不代表已调用真实外部模型。参考 bundle + 桩桥测试仍见 `crates/api/tests/agent_runtime.rs`、`crates/worker/tests/host_ops.rs`。

### W00 Agent PostgreSQL 持久化

- `migrations/0007_agent_state.sql`（9 张表）与 `PgAgentRepository`；由 fail-closed 桩实现为完整实现，**未添加内存回退**，内存模式（不设 `DATABASE_URL`）不受影响。
- 会话行 `SELECT … FOR UPDATE` 是幂等重放、单活跃 turn、消息序号与事件游标的串行点；`cancel_turn` 锁 run 行使取消与完成成为一次串行判定。
- PostgreSQL 条件测试已用一次性容器实库执行通过（14/14），覆盖租户/项目隔离、幂等与同键异请求冲突、Run 状态机、run 原子声明与单次完成、checkpoint 往返与输入变更冲突、ToolCallLedger 幂等追加、并发事件序号单调、重启重放、取消竞争、仅附件消息。另有一项 250ms 超时指向 `127.0.0.1:1` 的常驻 fail-closed 测试，无需凭据。

相关入口：

- op 面与 trait 边界：`crates/worker/src/host.rs`
- op 实现与生产运行时：`crates/worker/src/host_ops.rs`、`crates/worker/src/host_runtime.rs`
- API 侧能力实现与装配：`crates/api/src/agent_runtime.rs`、`crates/app/src/main.rs`
- Agent 持久化：`migrations/0007_agent_state.sql`、`crates/persistence/src/agent.rs`
- 实库测试：`crates/persistence/tests/postgres.rs`

### W09 报告 fan-in 首批实现

- `crates/domain/src/report.rs`：冻结分母、逐文档/发布/独立测量状态汇聚、证据归属/时间校验、缺口结论与不可变更正；缺测不算未提及，未知发布不算失败。
- `crates/persistence/src/report.rs` 与迁移 `0009_report_snapshots.sql`：租户作用域下保存快照，重复生成复用结果；更正创建关联的新版本。
- `crates/api/src/reports.rs`：列表、详情、证据与周期 reduce 服务。PostgreSQL 定时扫描到期周期，并恢复首次报告已存但后继未建的周期；两条扫描均使用游标分页。新周期创建不等于完整下一轮内容与发布执行。
- P14 已接列表、详情、证据、项目时区及 CSV；P00 注册 `report_get`/`report_reduce`，省略 ID 时在 Rust 中解析当前项目周期或最新快照。
- 应用已接已有文档规划、周期清单和渠道执行账本中的发布/测量目标；未建立计划显示不可用，缺测与未知结果保留。真实平台账号和 AI 搜索采样尚未验收，不能把夹具当成实际效果。文档状态无截止时间证明时仍保留时间依据缺口，不回填伪时间。
- 正式周报遵循冻结截止；随时生成的临时预览、PDF、下一周期动作及真实跨平台效果报告仍未完成。准确测试和 CI 提交对应关系见 `WORKLOG.md`。

## 当前账号与发布纵切

- 两类接入共用后台能力：运营人员在 `/ops/channels` 登录账号、分组并分配到项目；用户在项目 `/channels` 登录自己的账号。持久 session 在服务端加密存储，不存入前端 localStorage，不向客户返回运营池凭据。只需必要的首次登录/失效重连，没有逐篇发布审批。
- 新增后台 dispatcher：配置 runner 时，每 5 秒分页发现到期且从未尝试的目标；账号缺失、项目暂停、来源不可公开或 runner 不可用时延后，不消耗发布尝试。账号间并行；同一运营商账号通过持久预检租约及原子 claim 防止多进程争抢。迁移 `0013`，未知发送保留租约至到期且不会重发；自动外部查回仍未实现。尚无每秒百次真实发布容量验证。
- P00 已注册渠道发现、冻结计划、分页查状态及单目标执行工具，应用装配复用 HTTP 的 Rust 业务服务；正常排期由 dispatcher 执行，不要求模型逐条调用。`ChannelPlan` 不冒充完整文档×平台领域清单；第一层正文生成已另行接入，但从生成版本自动展开渠道目标仍未完成。生成 bundle 测试使用注入模型，不代表真实平台发布。
- 首个发布验收使用外部创作者平台账号，不要求客户部署 CMS/站群。冻结规格未改写；当前优先级以 `TODO.md` 为准。
- 项目账号入口为 `/app/:tenantId/:projectId/channels`；运营账号池入口为 `/ops/channels`。两者共用远程网页登录及加密 session，池账号通过项目分配供客户后台使用，不向客户返回池凭据。
- 新模块为 `channels`（domain/persistence/api）、`channel_jobs`（冻结目标、发送前尝试账本及报告输入）和 `packages/browser-runner`（隔离 Chromium）。迁移为 `0010`、`0011`；提交 `3f1ddb5` 已通过 Linux/Windows、固定 Chromium 夹具及新增 PostgreSQL 回归，真实平台账号验收仍未完成。
- P12 `/publications` 已接来源版本渠道账本和独立的正文版本分发覆盖页。既有渠道计划仍从公开 TXT/Markdown 来源创建；生成版本另由新分发 outbox 物化为独立渠道目标，不占用既有测量计划。发送结果未知时不允许盲目重发，自动查回与真实跨周期资产查回仍待实现。
- 新增周期纵切：`GET /projects/{id}/cycles/current` 与 `POST /projects/{id}/cycles` 独立于不可变首次启动记录。首次周报保存后尝试创建相邻下一自然周；数据库扫描会补建“报告已存、后继未建”的周期，暂停/归档项目不推进。P12 读取当前周期；新周期只创建未封存清单骨架，不复制旧发布任务、不假装已生成下一轮内容。本批验证结果见工作日志，迁移为 `0012`。
- 部署通过服务端 `GEO_BROWSER_RUNNER_URL` / `GEO_BROWSER_RUNNER_TOKEN` 连接 runner；`GEO_CHANNEL_SECRET_KEY` 为持久加密的 64 位十六进制密钥，重启必须保留。缺少密钥只使持久凭据操作不可用，不阻断其他页面。`GEO_OPERATOR_POOL_TENANT_ID` 指定资源管理租户，池管理还需该租户内的资源/运营管理员成员身份。
- 本地内存账号与临时加密密钥在重启后失效，不能证明持久 session 恢复。真实平台身份、编辑器、发布与公开读回尚未完成账号实测；网页 AI 搜索与引用也未实测。来源推导的选择器、模拟回执或 OAuth 登录不能证明真实渠道可用。

## 当前正文生成纵切

- `domain/content.rs` 和 `persistence/content.rs` 保存独立执行、步骤租约/尝试、证据简报、结构化正文版本、检查及交接快照（迁移 `0014`）；冻结文档规划清单不被执行进度覆写。
- `api/content.rs` 负责冻结来源范围和当前公开用途校验、生成与独立检查的单次模型变换；MemeLoop 原生 `agent-agent-loop` 在单独批准的 bundle 中分页编排。基础引用检查不是完整事实保证；产品/结构化事实提取及完整品牌策略仍缺失。
- P00 通过 `content_start`/`content_execution_read` 启动及查询；当前周期尚无封存清单时，启动会读取冻结周期配置与当前知识版本并规划封存，已有封存版本不重新规划。P08/P09 已接执行、证据、正文版本和乐观并发编辑；仍无无需用户触发的整个周期自动启动及两轮自动修正。
- PostgreSQL 内容运行新增独立调度租约与 fencing、续租/失租中止、分页扫描和失败退避（迁移 `0016`）。配置内容 bundle 与模型后后台扫描运行中的内容执行；它不是 P00 Agent 回合的 queued 恢复、checkpoint 或工具副作用对账。
- 第一层就绪正文已可冻结为单独的第二层分发执行清单（迁移 `0017`）：按已冻结文档版本与平台范围物化文档×平台覆盖、确定性渠道变体、逻辑发布意图和持久 outbox，支持分页恢复、范围/来源重新核验及 P12 查阅。迁移 `0018` 已接 outbox 消费者与现有后台扫描，报告已采用正式覆盖分母；目标关联回执的报告投影、真实发布与完整自动运营仍未完成，不能宣称两层发布闭环。
- 原生 V8 与 AppState/ContentService 测试使用注入模型；新增内容恢复与分发实库回归以 `WORKLOG.md` 的 CI 记录为准，不代表真实外部模型或平台验收。

## 4. 仍未实现，禁止误判为完成

- 真实 MemeLoop bundle 单回合、附件导入/知识检索工具循环以及独立的原生第一层内容分支工作流已有嵌入式 V8 验证；内容 PostgreSQL 扫描/续租不等于 Agent 回合恢复，第二层仍须接真实发布、测量与 reduce 编排。构建入口 `pnpm agent:bundle`，产物不提交。
- **重启对账**：已接 `GEO_SINGLE_PROCESS_EXECUTOR=true` 启动扫描，只适用于整个数据库严格单执行进程，默认关闭。滚动部署、多副本不得启用；queued 恢复、租约和优雅关闭仍未实现。PostgreSQL 对账测试已由 CI 独立 schema 验收。
- **回合进行中的实时取消**：`cancel_turn` 语义正确（`finish_run` 不会覆盖 `Cancelled`），但取消不触达隔离体，turn 仍跑到 deadline 才结束。`HostBridge::with_cancellation` 已备好接口。
- **隔离体基础保护**：64 MiB V8 堆、near-heap 终止、独立墙钟和 Rust 输出预算均已回归通过。不设固定隔离体并发准入门槛；高吞吐调度和进程资源观测仍需真实容量验收。
- **checkpoint 与 tool-call ledger**：executor 已写完成结果存档，中途恢复及 Rust 调用侧 intent/attempt/outcome 尚未接通。
- Token Center HTTP 适配及持久租户/项目路由已装配，配置撤销和凭据 generation 在调用时重新校验；正式权益/费用记账、流式模型事件及真实部署验证仍未完成。本地单模型配置保持仅限内存开发模式。
- 对话 registry 已注册渠道计划/执行及第一层内容启动/查询工具；周期启动时自动封存规划已接，第二层分发清单尚无 P00 工具及发送编排，真实搜索测量仍待补齐，不能用既有渠道计划替代新分发清单。
- P00 非 TXT/Markdown 解析、媒体/表格与跨回合附件使用；当前存储为内存或 PostgreSQL blob，非正式对象存储服务。回合输入可由持久 Message 重建，但这不等于完整自动恢复执行。
- 项目级知识文档清单已支持规划、封存和只读 GET，P07 显示真实覆盖项及来源依赖。内容启动在当前周期无清单时按冻结配置规划，刷新封存清单不重新规划，知识当前版本改变也不覆盖历史清单。已有独立正文生成与分发覆盖矩阵；产品级细化、分发实际执行及真实连接器/账号池/出口池验收仍待完成。
- 独立 AI 渠道采样与真实发布证据接入、丰富效果归纳及自动进入下一轮；已有报告快照/汇聚器和到期周期扫描，不代表整个周闭环完成。
- PostgreSQL 全仓库事务级 tenant scope、FORCE RLS 和非 bypass 角色验收。

PostgreSQL 模式下 `PgAgentRepository` 已实现持久化，数据库故障仍 fail closed，不能回退内存。完成结果存档不代表中途恢复或工具副作用账本。复制 `.env.example` 不会自动把变量载入 Rust 进程，PowerShell 中需要显式设置环境变量。

## 5. 本地启动

数据库首次身份初始化、持久租户模型路由和独立原生内容 bundle 的新增部署入口见 [`runtime-deployment.md`](runtime-deployment.md)。相关能力处于本批集成，精确验证状态以工作日志为准；不能沿用下面旧开发路径的测试结论替代持久部署验收。

### 可选：启用本地 P00 模型单回合

仅允许未配置 `DATABASE_URL` 且监听 loopback 的本地内存模式。
正式租户 Token Center 接入另见 `token-center-integration.md`，不能使用此模式代替。

先运行 `pnpm agent:bundle`，再同时提供全部五项服务端环境变量：
`GEO_AI_BASE_URL`、`GEO_AI_API_KEY`、`GEO_AI_MODEL`、
`GEO_AGENT_BUNDLE_PATH`、`GEO_AGENT_BUNDLE_SHA256`。
密钥通过运行环境或密钥管理器注入，不写入命令日志、Git 或 VITE 变量。
产物路径为 `packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs`；
摘要可用 `Get-FileHash -Algorithm SHA256` 计算。重新构建后需更新摘要。

全部缺省时保持未配置；只提供部分配置、摘要错误、文件超过 8 MiB 或尝试用于数据库模式均拒绝启动。
模型路由固定为配置模型；该阶段仅证明真实 MemeLoop 单回合模型路径，
已支持 TXT/Markdown 附件导入及知识检索工具循环，但不代表多回合恢复、发布和测量工具已接通。不设置固定两回合并发门禁，优先跑通应用功能。

无需真实凭据的装配验证：
`cargo test -p geo-app generated_bundle_runs_one_turn_through_assembled_provider -- --ignored`。
它使用真实生成 bundle 与测试 transport，不会调用外部 AI。

### 最快的内存开发模式

内存模式适合 UI/API 开发；重启进程会丢失数据。不要设置 `DATABASE_URL`。

```powershell
pnpm install
$env:GEO_DEV_PASSWORD = "local-dev-password"
Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
cargo run -p geo-app
```

另开终端：

```powershell
pnpm --dir apps/web dev --host 127.0.0.1
```

浏览器打开 `http://127.0.0.1:5173`，用户名为 `demo@localhost`，密码是当前终端设置的 `GEO_DEV_PASSWORD`。

### PostgreSQL 模式

```powershell
Copy-Item .env.example .env
docker compose up -d postgres
$env:DATABASE_URL = "postgres://memeloop:change-me-local-only@localhost:5432/memeloop"
cargo run -p geo-app
```

启动会自动执行 `migrations/`。数据库迁移不创建演示用户、Operator Host 或 Membership；部署者需先按 [`runtime-deployment.md`](runtime-deployment.md) 执行显式 `--bootstrap`，再配置持久租户/项目模型路由和经摘要校验的 bundle。无有效模型路由时 P00 明确记录 `capability_missing`，不会生成伪 AI 回答；有路由也不代表权益、费用、真实外部模型或平台渠道已验收。

完整基础设施定义见 `compose.yaml`，示例变量见 `.env.example`。任何模型、Token Center、客户、账号或代理凭据都只能由本地环境或秘密管理器注入，禁止写入仓库、前端变量、测试快照和日志。

## 6. 验证命令

提交前至少运行：

```powershell
pnpm format:check
pnpm typecheck
pnpm test
pnpm build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

PostgreSQL 条件测试需要一个可丢弃的空数据库：

```powershell
$env:GEO_TEST_DATABASE_URL = "postgres://..."
cargo test -p geo-persistence --test postgres -- --ignored
```

测试会执行迁移并写入数据，不要指向共享、生产或含重要数据的数据库。

## 7. 下一项工作的明确入口

当前最高优先级是 `TODO.md` 中的 W00，不要先扩展次要页面。建议按以下可独立提交的顺序推进：

1. **本批验证收口**：`e8ba567` 已通过 Linux/Windows、前端、固定浏览器、显式 bundle 和全部配置的 PostgreSQL 套件；后续代码必须重新验证，不能借用该提交的结果。
2. **第一层内容补全**：当前周期启动可按冻结配置规划封存，PostgreSQL 内容扫描、续租和失租中止已接；补两轮自动修正、故障注入和无需手动触发的跨周期自动运营。
3. **run executor 恢复**：Agent 回合驱动链已有，但多副本租约/queued 恢复、实时取消及持久 checkpoint/tool-call ledger 仍独立缺失。不得借用内容执行的租约宣称 Agent 回合已恢复。
4. **第二层与汇聚**：分发覆盖/变体/意图/outbox、消费者和发送前 attempt 已建立；继续接真实账号发布/公开查回、P00 和目标关联报告证据、独立 AI 搜索测量及下一轮增量，保留完整证据和分母。
5. **首个完整纵切**：附件显式导入和带来源回答已通；仍须验证两个文档分支的进程重启恢复、结果汇总及全链路作用域/费用约束。

当前附件导入的具体接缝、文件责任和验收见
[`p00-attachment-import-slice.md`](p00-attachment-import-slice.md)。

每个提交都必须：

- 同时包含相应领域/API/持久化或 UI 测试。
- 不把未实现能力显示为成功。
- 保持 tenant/project scope、幂等、可恢复和证据链。
- 完成后精简 `TODO.md`，把事实、测试数量、浏览器验证与限制写入 `WORKLOG.md`。

## 8. 已知风险与设计边界

- 上游 MemeLoop 官方 server worker 是 Node 实现；真实 bundle 已在 Rust/V8 单回合运行，但不代表所有 Node 功能、完整工具循环及多回合恢复均已兼容。
- `@memeloop/react-ui` 使用 MUI/assistant-ui；只能在 P00 局部 ThemeProvider 中使用，不能污染 Fluent 全局主题。
- 当前前端生产构建存在大 chunk 警告，尚不阻塞功能，但后续应按路由拆分 P00 依赖。
- `migrations/0004_tenant_rls.sql` 仍是安全 no-op，不能对外宣称 FORCE RLS 已完成。
- NATS、Redis、MinIO 已有本地基础设施定义，但当前纵切的大部分状态仍直接由 API/repository 处理，不能仅凭 Compose 服务存在就宣称已接入。

## 9. 接手者十分钟检查

```powershell
git status --short
git log -5 --oneline
Get-Content docs/TODO.md
pnpm install --frozen-lockfile
pnpm typecheck
cargo test --workspace --all-targets
```

预期：当前集成工作区可能仍有尚未提交的并行改动；先确认其归属，不能将它们当作待清理文件。前端类型检查和不依赖外部 PostgreSQL 的常规测试应通过；显式 bundle、实库与真实渠道需分别验收。若结果不同，先记录环境与失败证据，再修改代码，不要根据旧聊天记录覆盖仓库现状。

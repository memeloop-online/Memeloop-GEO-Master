# Memeloop GEO 工程交接说明

更新时间：2026-10-10

## 当前状态（2026-10-10）

- PR #6 精确 HEAD `919102c` 全部检查通过，已合入 `main`（`00abe6e`）。固定诊断 API／Web 均 Ready，健康检查 HTTP 200、`durable=true`；API 启动一次数据库池超时重启后恢复，执行器 `99dd02f` 零重启。API／供应方 info 已启用；恰好一次已存来源重解析成功完成并核验，整页刷新后同版本、答案与六条引用持久读回，未新增消费端问题。
- 实际项目设置 `/settings?tab=ai` 已可见，工作台和解析默认模型均读回 `gpt-6.1-sol`，此前实际已保存配置的连接测试成功。超时 unknown 回执的来源筛选修复已在真实页面确认，已存原文现可发现；自定义密钥保存/轮换、新实时解析回调和最新预算仍须真实验收。入口见 [`project-ai-settings.md`](project-ai-settings.md)。
- 已核对部署启用 `GEO_SCOPED_TEST_AI=true`，不能以生产路由数量为零解释旧超时。本次成功解析实际模型为 `gpt-6.1-sol`，不外推其他模型可用性；旧 60 秒超时根因仍未证明，单次成功不代表稳定性已验收。
- 已存原文重分析及信源真实页面、答案／引用与刷新持久读回已通过，原测量 unknown 和旧失败版本保持不变。随后在 `350bc23` 实际页面完成无周期独立报告预览、保存、整页刷新与同版本再次读取，未新增外部问题或创建周期。这不证明旧失败根因，也不代表新实时采样、P00 同报告调用或共同搜索报告已验收。入口见 [`saved-observation-analysis.md`](saved-observation-analysis.md)。
- 已知系统会话的后台自然 reconcile 已自动终结为 Deleted，未人工调用 delete，也未新增外部提问。该结果证明这一会话的严格不存在对账与本地终态收敛，不代表全部历史会话已清理；身份、会话归属、完整库存与先前未知删除尝试的验证边界保持不变。
- 本次重解析在 21.350536 秒完成原文核验，实际模型、时间及版本与信源回链一致；六条引用的 URL／用途审计、完成状态及搜索证据已在 UI 核对，信源聚合新增六个域名，独立分析计数由一增至二。原执行 Unknown 与旧失败版本保留，远端会话已自然 Deleted。成功仅覆盖此已存来源案例，不证明旧超时根因、实时多话题恢复或长期稳定性。
- 多供应商身份入口已上线，但三家新增渠道尚无真实账号验收或完整联网适配。独立 SSE 采集模块已通过真实 Chromium／本地回环 CDP 一次流式采集，未据此开放或宣称测量能力。全部已跟踪执行器测试 246/246 通过、零跳过；合成测试不替代真实供应商验收。每次真实采样前仍须完整验证账号、供应商身份、模型、会话与权限／租约。
- 快速完成原始流的有界读回 `e1ba43d` 已包含在 PR #6；11/11、相关 58/58、真实 Chrome 两种路径及审查通过，不等于真实供应商支持。账号文案 `70dcc0e` 的 50/50 及审查通过，已部署。
- 桌面 registry 缓存后继构建已验证 apt 层实际命中，未出现缺失 blob 或 apt 回退；npm 步骤仍执行，不能宣称全部层缓存命中。API/Web 已按输入分别构建。富文本编辑桥 CI 成功，但未启用外部发布能力，也无实际发布证明。
- PR #7 共同报告已合入 main `7827b9e`，已核验并删除报告本地及远端分支。报告保存、整页刷新重开同版及搜索空样本已验证；真实搜索样本及 P00 同报告调用仍待验收。PR #8 时序修复与 PR #9 解析诊断各自等待精确提交 CI／合并；受保护输入法实验保留。
- 当前供应商生命周期适配增量只抽出 Kimi 原有传输证明：静态注册表从调用方显式选择供应商，缺失或未知供应商不生成所有权／完成证明。原始内容仍由 AI 解释；其他供应商未因此取得测量或清理能力。代码入口为 `packages/browser-runner/src/provider-conversation-ownership.mjs` 与 `kimi-conversation-evidence.mjs`。

历史部署、失败修复和逐批验证只查 [`WORKLOG.md`](WORKLOG.md)，不将旧批次“待提交/待部署”视为当前状态。完整未完成范围和验收门槛见 [`TODO.md`](TODO.md)；产品基线保持不变。

资源边界：源码、构建缓存和测试产物均留在用户指定的工作卷；不得未经授权改用其他卷。历史越界产物清理尚未完成，恢复构建前先核对目标、临时目录和工具缓存配置。

未验证的输入法实验继续保留在工作区，不得误提交或视为已完成。实际域名、凭据及运维配置只从受控部署配置读取，不写入公开交接文档。

- PR #6、#7 已合并；旧 PR #1–#5 经祖先及补丁等价核对后关闭，已合并引用已清理，历史可从 Git／main 恢复。后续增量独立评审、检查和合并，不重新开启已完成批次审查。
- Windows 开发复用 CI 的预编译 V8 静态库，恢复和校验方法见 [README](../README.md)，不需要重新编译 V8 源码。
- P00 当前源码及已部署 API 均为 `geo.hostops.v19`，48 项工具；包含独立窗口报告及搜索创建／读取／已存响应重解析，bundle 与实际摘要已核对匹配。独立报告和搜索接口边界见 [`measurement-period-and-search.md`](measurement-period-and-search.md)。媒体工具、正文版本定点查询与缩略图增量已有前批验证，详细证据见 [`WORKLOG.md`](WORKLOG.md)，媒体接口边界见 [`p00-content-media-tools.md`](p00-content-media-tools.md)。完整历史列表和写事务仍保留原聚合路径，不能据此宣称整体容量验收通过。

本文只保留当前状态、代码入口、运行方法及边界；旧批次时间线与失败/修复过程由 [`WORKLOG.md`](WORKLOG.md) 保存，未完成事项以 [`TODO.md`](TODO.md) 为准。无需依赖聊天历史接手。

集群演示打包入口见 [`cluster-demo.md`](cluster-demo.md)：根 `Dockerfile`、`deploy/demo/Dockerfile.web`、`deploy/demo/kubernetes.yaml`、`deploy/demo/browser-runner.yaml` 和两项镜像 CI。清单不包含真实 Secret 值或域名。

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
- 已有基线支持 `text/plain`、`text/markdown` 与 UTF-8、逗号分隔、含表头 CSV，输入契约见 `csv-import.md`。PDF 文字层解析支持原件入队、独立 Tika/PDFBox 子进程、持久逐页进度、部分知识版本及失败页重试；P03/P04 和 P00 显式附件导入复用同一后台链路。DOCX/XLSX 已接独立配置的 POI 结构解析、持久单元进度、版本/重试及证据表格；对应验证记录见工作日志。启动设置及边界见 [`pdf-import.md`](pdf-import.md)、[`office-import.md`](office-import.md)。不配置对应解析服务时明确不可用；网页抓取、OCR、原件高亮预览、向量检索、LLM 知识回答和结构化事实提取尚未完成。

### P00 AI 工作台

- P00 是侧边栏首项，项目根、项目切换和首次启动完成后默认进入 `/chat`。
- 局部集成 `@memeloop/react-ui` 的 `AgentChatView`，外围应用壳继续使用 Fluent UI。
- Conversation、Message、Turn、Run、AttachmentReference、RuntimeCapability 和递增 ConversationEvent 已接内存与 PostgreSQL 持久实现。
- 已有会话创建/列表/详情、消息提交、Turn 取消和 SSE 重放 API；支持幂等提交、同键异请求冲突、附件-only 消息、跨项目隔离、`after`/`Last-Event-ID` 恢复。
- 多回合历史从同作用域持久消息重建当前序号之前的成功问答对，最近最多 20 对且 JSON UTF-8 不超过 128 KiB；整对省略并向模型报告省略数量，不生成虚构摘要。原生 MemeLoop 每轮独立恢复消息并分页读取，历史不授予附件导入权限。执行器使用有界 PostgreSQL 历史读取，HTTP 历史详情仍独立读取完整会话；SQL 保留完整合格回合计数以计算省略数量。此项不等于中途 checkpoint 恢复。
- P00 已接多附件选择/拖拽/粘贴、逐项上传及失败重试；专用附件 API 核验字节与摘要，提交消息时核对作用域和已提交对象元数据。TXT/Markdown 可经模型显式导入工具形成知识版本，再检索并引用回答；上传本身不直接入库，原始大文件不塞入模型上下文。
- 真实 MemeLoop 引擎已通过注入模型的“模型 → `knowledge_search` → Rust 检索 → 模型回答”回归；JSON function-tool 协议和有界多回合历史已接入，不代替外部模型验收，尚无流式工具分片或中途恢复。
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
- 隔离边界：底层 JS/Rust 通道有 4 个窄桥接原语，业务能力另由 48 项封闭且带版本的 Host Ops 提供（见下节）；模块仅限 Rust 注入的内存 allow-list，无文件系统、网络或包 registry 解析；checkpoint 是 Rust 拥有的宿主状态序列化，不是 V8 堆快照。
- 回退路径不需要第二套运行时：`deno_core` 自带 `quickjs` feature，可在同一 API 上切换引擎。

相关入口：

- 探针运行时：`crates/worker/src/runtime.rs`
- 内存模块加载器：`crates/worker/src/loader.rs`
- 窄 host op：`crates/worker/src/ops.rs`
- 最小 bundle：`crates/worker/src/bundle.rs`
- 探针测试：`crates/worker/tests/probe.rs`

### W00 安全 Host Ops

- 封闭且带版本的 op 面：代码 `crates/worker/src/host.rs` 当前为 `geo.hostops.v19`、48 项（含单篇分发、独立窗口报告及传统搜索三项工具；以 `HOST_OPS_VERSION` 与 `HostOp::COUNT` 为准）。问题发现不返回冻结评估正文；独立测量仅对可验证来自专用临时问题命令的真实结果返回受限原始回答/引用，冻结评估、旧计划和夹具不暴露此投影。搜索工具不返回原始响应或凭据，非优化用途细节保持隔离。部署须同步新 bundle 及摘要。附件导入只接受 Rust 已绑定到当前回合的对象；业务工具只传受限资源引用，JS 无法取得 session、代理凭据、SQL、任意网络、文件、进程或环境变量。
- 边界方向为 `geo-api → geo-worker`，worker 从不反向依赖 API。请求 DTO 全部 `#[serde(deny_unknown_fields)]` 且不携带 tenant/project 选择器，作用域只能来自 Rust 侧 bridge。预算、单次调用截止与取消统一在 `HostBridge::invoke` 施加。
- `RepositoryHostOps` 已实现知识检索/附件导入、文档清单读取、报告及渠道工具和可注入的模型调用；内容工具另由受限 Rust 服务执行。本地开发模型装配见第 5 节。清单读取保留规划状态、阻断原因及覆盖分母，不把规划项 ID 冒充正文版本；正式文档×平台展开和正文分发工具已接入，真实发布与测量验收仍缺失。

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

### 周期自动准备与取消

- 迁移 `0019` 为当前活跃周期添加持久 bootstrap claim、重试时间与稳定错误码。配置内容 bundle 和模型后，后台每 30 秒分页发现尚无内容执行的周期；创建时再次校验项目未暂停且周期仍为当前周期。
- 原生 MemeLoop 内容工作流在封存后继续第二层清单展开，并另行遍历全部目标进行物化/恢复；只有 `distribution.prepared` 才表示本次准备完成。该事件不表示发布成功，也不表示独立测量完成。
- 已封存执行依旧绑定原周期；恢复不切到项目新周期，不重新生成正文。闭合阶段可在没有模型时恢复，共享原持久执行租约；暂时依赖缺失后退避再查，未知/已发送意图不被当作新发送。
- P00 取消保存后通知本进程运行；跨副本以作用域内单运行状态查询检查。取消触达 host bridge、挂起的模型调用和同步 JavaScript 循环，新 turn 使用独立取消状态。已发生外部副作用不能撤销；未知发送查回另由独立扫描器处理，边界见上文。
- 对应本地、PostgreSQL 与 CI 证据以 `WORKLOG.md` 为准；不要把既有绿色基线套用于未提交修改。

### W09 报告 fan-in 首批实现

- `crates/domain/src/report.rs`：冻结分母、逐文档/发布/独立测量状态汇聚、证据归属/时间校验、缺口结论与不可变更正；缺测不算未提及，未知发布不算失败。
- `crates/persistence/src/report.rs` 与迁移 `0009_report_snapshots.sql`：租户作用域下保存快照，重复生成复用结果；更正创建关联的新版本。
- `crates/api/src/reports.rs`：列表、详情、证据与周期 reduce 服务。PostgreSQL 定时扫描到期周期，并恢复首次报告已存但后继未建的周期；两条扫描均使用游标分页。新周期创建不等于完整下一轮内容与发布执行。
- P14 已接列表、详情、证据、项目时区及 CSV；P00 注册 `report_get`/`report_reduce`，省略 ID 时在 Rust 中解析当前项目周期或最新快照。
- 应用已接已有文档规划、周期清单和渠道执行账本中的发布/测量目标；未建立计划显示不可用，缺测与未知结果保留。真实发布及跨渠道报告仍待验收，既有单个网页样本不代表完整效果闭环。文档状态无截止时间证明时仍保留时间依据缺口，不回填伪时间。
- 正式周报遵循冻结截止；独立只读临时预览的 P14/API/P00 入口将证据时间截断至冻结截止，不保存正式快照或推进周期，实施契约见 [`report-preview.md`](report-preview.md)。正式快照 PDF 导出和用途分类标签已接入；下一周期优化动作及真实跨平台效果报告仍未完成。准确测试和 CI 对应关系见 `WORKLOG.md`，不能沿用旧绿色基线。

## 当前账号与发布纵切

- 两类接入共用后台能力：运营人员在 `/ops/channels` 登录账号、分组并分配到项目；用户在项目 `/channels` 登录自己的账号。持久 session 在服务端加密存储，不存入前端 localStorage，不向客户返回运营池凭据。只需必要的首次登录/失效重连，没有逐篇发布审批。
- 新增后台 dispatcher：配置 runner 时，每 5 秒分页发现到期且从未尝试的目标；账号缺失、项目暂停、来源不可公开或 runner 不可用时延后，不消耗发布尝试。账号间并行；同一运营商账号通过持久预检租约及原子 claim 防止多进程争抢。迁移 `0013`，未知发送保留租约至到期且不会重发；自动只读查回另接独立账本，不升级原发送结果。尚无每秒百次真实发布容量验证。
- P00 已注册渠道发现、冻结计划、分页查状态及单目标执行工具，应用装配复用 HTTP 的 Rust 业务服务；正常排期由 dispatcher 执行，不要求模型逐条调用。`ChannelPlan` 不冒充完整文档×平台领域清单；第一层正文生成、正式分发清单与生成版本渠道目标已通过独立 outbox 接入。生成 bundle 测试使用注入模型，不代表真实平台发布。
- 首个发布验收使用外部创作者平台账号，不要求客户部署 CMS/站群。冻结规格未改写；当前优先级以 `TODO.md` 为准。
- 项目账号入口为 `/app/:tenantId/:projectId/channels`；运营账号池入口为 `/ops/channels`。两者共用远程网页登录及加密 session，池账号通过项目分配供客户后台使用，不向客户返回池凭据。
- 新模块为 `channels`（domain/persistence/api）、`channel_jobs`（冻结目标、发送前尝试账本及报告输入）和 `packages/browser-runner`（隔离 Chromium）。迁移为 `0010`、`0011`；提交 `3f1ddb5` 已通过 Linux/Windows、固定 Chromium 夹具及新增 PostgreSQL 回归，真实平台账号验收仍未完成。
- P12 `/publications` 已接来源版本渠道账本和独立的正文版本分发覆盖页。既有渠道计划仍从公开 TXT/Markdown 来源创建；生成版本另由新分发 outbox 物化为独立渠道目标，不占用既有测量计划。发送结果未知时不允许盲目重发，后台自动查回、本轮观察读取及跨周期原目标导航已接线；真实渠道查回验收仍缺。
- 周期纵切：`GET /projects/{id}/cycles/current` 与 `POST /projects/{id}/cycles` 独立于不可变首次启动记录。首次周报保存后尝试创建相邻下一自然周；数据库扫描会补建“报告已存、后继未建”的周期，暂停/归档项目不推进。P12 读取当前周期；新周期只创建未封存清单骨架，不复制旧发布任务、不假装已生成下一轮内容。验证结果见工作日志，迁移为 `0012`。
- 部署通过服务端 `GEO_BROWSER_RUNNER_URL` / `GEO_BROWSER_RUNNER_TOKEN` 连接 runner；`GEO_CHANNEL_SECRET_KEY` 为持久加密的 64 位十六进制密钥，重启必须保留。缺少密钥只使持久凭据操作不可用，不阻断其他页面。`GEO_OPERATOR_POOL_TENANT_ID` 指定资源管理租户，池管理还需该租户内的资源/运营管理员成员身份。
- 本地内存账号与临时加密密钥在重启后失效，不能证明持久 session 恢复。已有消费端账号恢复、模型发现与网页引用样本证据；创作者平台身份、编辑器、发布与公开读回仍须分别实测。来源推导的选择器、模拟回执或 OAuth 登录不能证明真实发布可用。

## 当前正文生成纵切

- `domain/content.rs` 和 `persistence/content.rs` 保存独立执行、步骤租约/尝试、证据简报、结构化正文版本、检查及交接快照（迁移 `0014`）；冻结文档规划清单不被执行进度覆写。
- `api/content.rs` 负责冻结来源范围和当前公开用途校验、生成与独立检查的单次模型变换；MemeLoop 原生 `agent-agent-loop` 在单独批准的 bundle 中分页编排。基础引用检查不是完整事实保证；产品/结构化事实提取及完整品牌策略仍缺失。
- P00 通过 `content_start`/`content_execution_read` 启动及查询；当前周期尚无封存清单时，启动会读取冻结周期配置与当前知识版本并规划封存，已有封存版本不重新规划。P08/P09 已接执行、证据、正文版本和乐观并发编辑；迁移 `0019` 已接周期自动准备。两轮自动事实修正已有持久计数、版本绑定租约与再次独立检查，验证记录见 `WORKLOG.md`；真实跨周期无人值守闭环仍未完成。
- PostgreSQL 内容运行新增独立调度租约与 fencing、续租/失租中止、分页扫描和失败退避（迁移 `0016`）。配置内容 bundle 与模型后后台扫描运行中的内容执行；它不是 P00 Agent 回合的 queued 恢复、checkpoint 或工具副作用对账。
- 第一层就绪正文已可冻结为单独的第二层分发执行清单（迁移 `0017`）：按已冻结文档版本与平台范围物化文档×平台覆盖、确定性渠道变体、逻辑发布意图和持久 outbox，支持分页恢复、范围/来源重新核验及 P12 查阅。迁移 `0018` 已接 outbox 消费者与现有后台扫描，报告采用正式覆盖分母；目标关联回执和公开验证报告投影已有 CI 证据，真实发布与完整自动运营仍未完成，不能宣称两层发布闭环。
- 原生 V8 与 AppState/ContentService 测试使用注入模型；新增内容恢复与分发实库回归以 `WORKLOG.md` 的 CI 记录为准，不代表真实外部模型或平台验收。

## 4. 仍未实现，禁止误判为完成

- 真实 MemeLoop bundle 单回合、附件导入/知识检索工具循环以及独立的原生第一层内容分支工作流已有嵌入式 V8 验证；内容 PostgreSQL 扫描/续租不等于 Agent 回合恢复，第二层仍须接真实发布、测量与 reduce 编排。构建入口 `pnpm agent:bundle`，产物不提交。
- **queued 恢复**：PostgreSQL 模式每 5 秒按 `(created_at, run_id)` 分页发现未开始回合，迁移 `0022` 提供部分索引；配置好的运行时通过与 HTTP 相同的 `begin_run` 原子领取，再从持久消息重建输入。扫描不携带提示词或附件，不领取 running/终态，无运行时则保持 queued。竞争、取消、重启和分页回归已有 CI 证据，不宣称已完成 running 中途恢复。
- **running 重启对账**：已接 `GEO_SINGLE_PROCESS_EXECUTOR=true` 启动扫描，只适用于整个数据库严格单执行进程，默认关闭。滚动部署、多副本不得启用；running 租约和优雅关闭仍未实现。此旧对账测试已由 CI 独立 schema 验收，不能替代新 queued 扫描验证。
- **回合进行中的实时取消**：隔离体及模型等待中断已接，独立回合取消状态不复用；生产多副本与在途外部结果核对仍须补验。
- **隔离体基础保护**：64 MiB V8 堆、near-heap 终止、独立墙钟和 Rust 输出预算均已回归通过。不设固定隔离体并发准入门槛；高吞吐调度和进程资源观测仍需真实容量验收。
- **checkpoint 与 tool-call ledger**：executor 已写完成结果存档；Rust 调用侧 intent/attempt/outcome 已通过既有本地及 CI 验证，内存与 PostgreSQL 共享生命周期，应用按实际 Run 注入仓储。问题集工具沿用账本并在保存成功前核对返回结果，独立验证状态见 `WORKLOG.md`；这不是中途恢复，跨副本 running 租约、稳定重入位置和可重放结果仍缺。边界见 [`agent-tool-ledger.md`](agent-tool-ledger.md)。
- Token Center HTTP 适配及持久租户/项目路由已装配，配置撤销和凭据 generation 在调用时重新校验；正式权益/费用记账、流式模型事件及真实部署验证仍未完成。本地单模型开发配置仅限回环内存模式或显式固定作用域的本地持久开发模式，不能充当正式租户路由。
- 对话 registry 已注册渠道计划/执行、第一层内容启动/查询及第二层分发启动/读取/恢复/分页工具；后者复用 `DistributionService`，不允许模型自报正文或平台能力。周期自动贯穿两次 fan-out 的原生工作流及多渠道真实测量仍待补齐，不能以工具存在或单个真实样本代替无人值守全周期运行。
- P00 已有图片媒体绑定、正文插入、富文本表格及选定版本 ZIP 导出，但不代表缩略图服务、富文本外部渠道适配或跨回合附件使用已完成；PDF/Office 异步解析仍须明确任务及状态，不可当作同步 TXT/Markdown 成功路径。当前存储为内存或 PostgreSQL blob，非正式对象存储服务。回合输入可由持久 Message 重建，但这不等于完整自动恢复执行。
- 项目级知识文档清单已支持规划、封存和只读 GET，P07 显示真实覆盖项及来源依赖。内容启动在当前周期无清单时按冻结配置规划，刷新封存清单不重新规划，知识当前版本改变也不覆盖历史清单。已有独立正文生成与分发覆盖矩阵；产品级细化、分发实际执行及真实连接器/账号池/出口池验收仍待完成。
- 独立 AI 渠道采样与真实发布证据接入、丰富效果归纳及自动进入下一轮；已有报告快照/汇聚器和到期周期扫描，不代表整个周闭环完成。
- PostgreSQL 全仓库事务级 tenant scope、FORCE RLS 和非 bypass 角色验收。

PostgreSQL 模式下 `PgAgentRepository` 已实现持久化，数据库故障仍 fail closed，不能回退内存。完成结果存档不代表中途恢复或工具副作用账本。复制 `.env.example` 不会自动把变量载入 Rust 进程，PowerShell 中需要显式设置环境变量。

## 5. 本地启动

数据库首次身份初始化、持久租户模型路由和独立原生内容 bundle 的部署入口见 [`runtime-deployment.md`](runtime-deployment.md)。精确验证状态以工作日志为准；不能沿用下面开发路径的测试结论替代持久部署验收。

持久本地接入辅助脚本见 [`local-workspace.md`](local-workspace.md)：监督专用 PostgreSQL、Rust API、浏览器 runner 与 Vite，私有配置必须位于仓库外。`--check` 使用真实浏览器检查首次软件登录和 Secure Cookie，`--interactive` 为用户保留可操作窗口；不添加产品认证旁路、不记录外部登录截图或凭据。实际持久启动和外部账号验收状态仍以工作日志为准。

### 可选：启用本地 P00 模型单回合

默认路径为未配置 `DATABASE_URL` 且监听 loopback 的本地内存模式。
持久本地演示另可显式启用固定运营商/租户的开发模式，具体变量和限制见 `runtime-deployment.md`；不必清空项目数据。
正式租户 Token Center 接入另见 `token-center-integration.md`，不能使用开发模式代替。

先运行 `pnpm agent:bundle`，再同时提供全部五项服务端环境变量：
`GEO_AI_BASE_URL`、`GEO_AI_API_KEY`、`GEO_AI_MODEL`、
`GEO_AGENT_BUNDLE_PATH`、`GEO_AGENT_BUNDLE_SHA256`。
密钥通过运行环境或密钥管理器注入，不写入命令日志、Git 或 VITE 变量。
产物路径为 `packages/agent-runtime/dist/memeloop-agent-loop.bundle.mjs`；
摘要可用 `Get-FileHash -Algorithm SHA256` 计算。重新构建后需更新摘要。

全部缺省时保持未配置；只提供部分配置、摘要错误、文件超过 8 MiB，或在数据库模式下未显式配置固定作用域开发模式时，均拒绝启动。
模型路由固定为配置模型；该阶段仅证明真实 MemeLoop 单回合模型路径，
已支持 TXT/Markdown 附件导入、知识检索工具循环及成功问答历史重建；发布与测量工具已有接线，但真实渠道尚未验收，中途 checkpoint 恢复仍缺。不设置固定两回合并发门禁，优先跑通应用功能。

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

### 实际浏览器冒烟

`scripts/verify-web-smoke.mjs` 使用实际本地内存 Rust API、Vite 和已安装的 Chromium，不拦截 API 为假响应。设置 `GEO_SMOKE_APP_BINARY` 为当前提交编译的应用绝对路径，`GEO_SMOKE_OUTPUT_DIR` 为仓库外截图目录，然后执行 `node scripts/verify-web-smoke.mjs`。可选 `PLAYWRIGHT_BROWSERS_PATH` 指定已有浏览器缓存、`GEO_SMOKE_TMP_DIR` 指定临时目录；脚本不下载浏览器。

脚本默认使用 8080/5173，也可通过 `GEO_SMOKE_API_PORT` / `GEO_SMOKE_WEB_PORT` 指定独立空闲端口。生成仅存在于子进程环境的随机开发密码，并以合成资料执行登录、启动项目、CSV 上传/证据、问题集创建/修订/历史、P12 未验证提示和 P14 临时预览。正常或失败时清理自己启动的服务，不停止其他服务。截图及失败诊断检查桌面/窄屏的根节点溢出与内部裁切，不能仅以无水平滚动判定布局可用。CI Linux 执行同一流程并上传合成截图。

可选 `GEO_SMOKE_CONTENT=1` 验证生成内容编辑链路；先运行 `pnpm agent:bundle` 生成批准的两份 bundle。脚本计算摘要、启动仅回环可达且使用随机临时密钥的确定性模型夹具，通过实际 Rust API 和 MemeLoop 内容工作流生成资产，再验证富文本编辑、版本读回和选定版本导出。`GEO_SMOKE_PROVIDER_PORT` 可指定独立空闲端口（默认 18081），不得占用正在使用的解析服务。模型输出仅取本轮导入的合成证据；不继承工作站模型凭据。此模式属于合成流程验收，不代表真实模型质量或外部渠道验收。

内容模式另需 Python 3 标准库 `zipfile`，用于读回实际浏览器下载的 ZIP 并核对目录、CRC、正文和原图字节，不解压任意文件路径。默认使用 Windows 的 `python`、其他平台的 `python3`，可用 `GEO_SMOKE_PYTHON` 指定可执行文件；CI 使用 runner 自带 Python，不另行引入 ZIP 解析实现。

这项验收不配置真实模型、外部账号、发布执行池或 PDF 解析器，因此不能证明真实发布、搜索、PDF 页面或生产持久部署已经验收。

## 7. 下一项工作的明确入口

当前最高优先级是 `TODO.md` 中的 W00，不要先扩展次要页面。建议按以下可独立提交的顺序推进：

1. **证据与会话生命周期**：已存原文重分析、信源及独立报告实际 UI 持久读回已通过，单个系统会话自然不存在对账已终结为 Deleted；继续验证其余合格历史与新增有界测量的完整执行身份。`provider-conversation-lifecycle.md` 是清理入口；完整库存、身份／会话、历史删除尝试及共享账号锁仍是边界。旧快照缺证明不得自动清理，不重新发送测量代替恢复。
2. **第一层内容补全**：当前周期启动可按冻结配置规划封存，PostgreSQL 内容扫描、续租和失租中止已接，两轮自动修正已有测试；继续真实模型验证、故障注入和无需手动触发的跨周期自动运营。
3. **run executor 恢复**：queued 分页发现/原子领取和实时取消已有验证；Rust-owned tool-call ledger 已接通（验证见工作日志），继续 running 多副本租约、中途 checkpoint、结果引用和未知调用核对。不得借用内容执行的租约宣称 Agent 回合已恢复。
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

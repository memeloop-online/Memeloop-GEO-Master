# 工作日志

## 2026-09-18

- 核对仓库：`main` 尚无提交，只有未入库的旧版方案。
- 旧方案含已否决的员工授权流程、过时的维基 403 结论，且缺少完整页面、知识库和资源池设计，因此不作为实施基线。
- 使用一次性短上下文 Astra 完成架构/API 审阅，补齐账号池、执行池、出口池、发布分配模型、策略实体、模块写权限和事件契约。
- 使用一次性短上下文 Astra 完成 UI/文案审阅，补齐批量接入、异常状态、无人审批交互和 Fluent UI 布局。
- 将正式 V1 产品与开发规格写入 `product-plan-v1.md`；后续冻结该文件。
- 完成 W01 工程基础：Rust workspace、Axum API、租户作用域、Operation、事件信封、SSE、JSON 命令幂等中间件、OpenAPI、React/Fluent 工作台、P01–P16 路由、P01 向导和 P02 总览。
- 建立 PostgreSQL/SQLx 持久化契约与初始迁移、PostgreSQL/NATS/Redis/MinIO 本地 Compose、AGPL-3.0-only、贡献说明和前后端 CI。
- 前端通过格式、类型、3 项组件测试和生产构建；后端通过格式、Clippy `-D warnings` 和 11 项测试；API 真实启动后健康检查与 OpenAPI 返回正常。
- 使用本地浏览器实机检查 P01/P02 布局、中文状态和 P01–P16 导航；移除开发运行时的 Fluent Keyborg 控制台错误。
- Docker Compose 配置验证通过；Docker Hub 拉取 PostgreSQL 镜像超时。已增加需显式 `GEO_TEST_DATABASE_URL` 的实库迁移测试，待镜像可用后执行。
- 用户提供此前未被读取的完整 1.0 产品规格。确认旧实施基线是缩减版本，暂停 W02 前后端代理，避免继续按错误范围推进。
- 使用一次性、无历史上下文的 Astra 将附件全部内容按原顺序恢复到 `product-plan-v1.md`；验证附件 972 条非空内容全部保留。
- 在完整规格中落实“两层 fan-out → reduce → 下一轮”：企业资料展开为有限版本化知识文档清单，文档再展开为全平台覆盖矩阵，最后汇聚发布证据和独立 AI 渠道测量，形成不可变周报及下一轮增量。
- 新增分支失败隔离、未知发布持续查回、周报截止快照、迟到证据版本、背压和跨阶段恢复要求；不因单个平台失败阻塞整轮，也不把跨平台数据混算为单一指标。
- 重新划定实施顺序：先补齐完整计划定义的 W01 登录会话、身份边界、API 客户端和异步状态，再按新 W02 的资料输入、预算估算与启动任务验收现有代码。

## 2026-09-19

- 完成 W01 同源 Cookie 会话：Argon2id 登录、不透明随机令牌、数据库仅存令牌哈希、CSRF 与 Origin 校验、Host 推导 Operator、Membership 校验 Tenant；旧身份请求头不能授权。
- 完成登录、工作区选择、退出、路由守卫、租户隔离查询键和统一加载/错误/空态；本地开发必须显式配置密码且只能监听回环地址。
- 接入项目列表、创建、详情、乐观并发更新、空数据总览、估算和启动 API；viewer 只读，普通 PATCH 不能绕过启动命令修改生命周期。
- P01 已能执行“服务端估算 → 创建草稿 → 启动 Operation”；创建成功而启动失败时只重试启动，并复用稳定幂等键。
- 启动 Operation 使用由租户、项目和幂等键推导的稳定 ID；并发同键请求、响应丢失和已持久化恢复标记均有测试覆盖。
- 真实浏览器完成“登录 → 选择工作区 → 四步配置 → 展示估算 → 创建并启动 → 总览”验收；总览明确区分“已启动”“等待知识处理”“尚未建立基线”，不伪称 W03 已解析资料。
- 统一 Fluent 依赖的 Keyborg 版本，消除同一页面加载两个不兼容主版本导致的焦点实例释放错误。
- 前端格式、类型检查、8 项测试和生产构建通过；Rust 格式、Clippy `-D warnings`、16 项测试通过，另有 1 项 PostgreSQL 实库测试因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- Astra 对照冻结计划审计 W02：现有估算的文档/平台/样本推导仍无真实能力快照依据；下一步改为三类独立且可为 unknown 的分母，并在同一事务创建配置版本、Cycle、两份未封存 manifest、Operation、Workflow 和 outbox。
- `0004_tenant_rls.sql` 仍是安全 no-op；在所有 repository 完成事务级 scope 和非 bypass 角色实测前，不宣称生产 RLS 已完成。
- 完成 W02 配置契约：项目时区、周报日/时间与截止、上一自然周窗口、文档范围、发布范围、目标、版本化来源引用；草稿允许不完整，启动使用独立严格校验。
- 周报窗口按项目时区的下一次周报触发计算，覆盖周日 23:59 截止、跨年和 DST 缺口；总览更新时间不再错误使用未来截止时间。
- 估算改为文档、文档×平台、AI 测量三类独立分母及三阶段费用；KnowledgeRelease、CapabilitySnapshot、MeasurementProtocol、PricingSnapshot 未形成时返回 unknown 和明确阻塞原因，不根据资料数量伪造覆盖。
- PostgreSQL `0005_atomic_project_start.sql` 与 repository 在同一事务创建 ProjectConfigRevision、OptimizationCycle、两份未封存 manifest、WorkflowRun、Operation、`cycle.created` outbox、启动记录和项目当前句柄；内存实现使用单锁并通过故障注入验证失败不留部分状态。
- 启动命令使用 `expected_revision`、稳定 Operation ID、哈希后的幂等键和请求哈希；同键同请求重放同一回执，同键异请求或不同键重复启动冲突；GET `/projects/:id/start` 支持刷新恢复。
- P01 收口为三步，草稿只创建一次并按 revision 串行自动保存；产品和目标用户可选且可通过 PATCH `null` 清空；P02 展示持久化配置、周期和两份 manifest 句柄。
- 文件上传 API 尚未实现，P01 明确提示且只提交真实可用的 URL、文本、对象引用和已有知识集合引用；上传会话与解析转入 W03。
- 真实浏览器完成“登录 → 工作区 → 三步配置 → unknown 估算 → 原子启动 → 总览 → 刷新恢复”验收；两份清单显示为未封存骨架，资料、基线和计划均未伪称完成，本轮无新增控制台错误。
- 前端格式、类型、11 项测试和生产构建通过；Rust 格式、Clippy `-D warnings`、13 项 API、1 项应用、3 项领域、3 项持久化测试及 doctest 通过。另有 2 项 PostgreSQL 条件测试因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- 完成 W03-A 企业知识库纵向切片：上传会话、原始字节核验、批量导入、来源/来源版本、确定性文本分段、不可变 KnowledgeRelease、可定位证据检索和 evidence-only 问答均具有内存与 PostgreSQL repository 契约。
- P01 文件入口已走“SHA-256 → 创建上传会话 → PUT 字节 → 完成核验”的真实 API；启动前将 URL、粘贴文本和已上传来源显式物化到知识库，重复执行不会复制已上传来源。
- P03—P05 已接入真实资料列表、来源详情、产品/事实空态、能力快照、当前知识版本、证据摘录模式和来源回链；未配置 LLM 时不会把检索片段伪装成完整回答。
- 浏览器实机回归发现知识 API 客户端漏传 `tenant_id`，导致资料物化报 `tenant selector is required`；修复为所有知识请求同时携带租户和项目资源选择器，并增加请求 URL 回归断言。
- 真实浏览器完成“登录 → 创建项目 → 粘贴公开资料 → 资料物化 → 启动 → 总览 → P03 → P04 → P05 查询‘质保’ → 回到来源详情”验收；总览显示知识版本已形成但文档清单仍待规划/封存，浏览器控制台无错误。
- W03-A 验证通过：Rust 格式、Clippy `-D warnings`、14 项 API、1 项应用、8 项领域、3 项持久化测试及 doctest；前端格式、类型、23 项测试和生产构建。2 项 PostgreSQL 条件测试仍因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- 当前能力边界如实暴露：只有 text/plain 与 text/markdown 能确定性解析；PDF/DOCX/XLSX/CSV、URL 抓取、OCR、向量、LLM 回答和结构化事实提取尚未接入；内存对象适配器不具备重启持久性。
- 用户明确将 P00 AI 工作台提升为最高优先级，并授权修改已冻结计划；产品规格升级为 1.1，新增对话/附件默认入口、MemeLoop 两层 fan-out/reduce 编排、Rust 托管 JavaScript Runtime、持久 Agent 状态与验收。
- 核对 `memeloop-online/memeloop`：上游为 MIT，具备 AgentToolLoop、AgentAgentLoop、`.mjs` 脚本、LoopProfile、权限、checkpoint 接口与 React UI adapter；GitHub 检查快照为 `7e9aec265a8870ec45cd186112d65108fb6b606d`。
- 同时核对当前 npm 精确版本 `memeloop@0.3.3` 与 `@memeloop/react-ui@0.2.3`；后者仍以 MUI/assistant-ui Web 组件为主，但提供窄入口、canonical adapter、附件与长会话投影能力。计划要求在 P00 局部组合主题，不污染 Fluent 应用壳。
- 明确运行时边界：MemeLoop 负责 Agent 与脚本编排；Rust 负责身份、权限、预算、清单、发布、测量、费用和外部副作用真相。JS 无 SQL、任意网络、文件、进程或环境变量权限。
- 停止尚未进入编码的 W03-B Astra 设计任务以节省成本；W03-A 已提交，后续从 P00 首个纵切恢复 W03 文档清单开发。
- P00 第一阶段基础已落地：项目根、项目选择和启动完成后默认进入 `/chat`；P00 固定为侧边栏第一项，局部 MUI ThemeProvider 内复用 `@memeloop/react-ui` 的 `AgentChatView`，保留 Fluent 应用壳。
- 新增项目隔离的 Conversation、Message、Turn、Run、AttachmentReference、RuntimeCapability 与递增 ConversationEvent 契约；内存仓库实现幂等消息受理、同键异请求冲突、仅附件消息、前序 turn、取消、事件重放和跨项目隔离。
- 新增 `/api/v1/agent/*` 会话、消息、取消与 SSE API；SSE 先订阅再重放以封闭丢事件窗口，支持 `after` 与 `Last-Event-ID`。未配置 Rust JS Runtime 时服务端结构化记录 `capability_missing`，前端只显示真实用户消息和明确提示，不伪造 AI 回答。
- PostgreSQL AgentRepository 目前明确 fail closed，尚未安装会话/checkpoint 迁移；附件选择只展示“需先上传为对象引用”，尚未接对象上传 adapter；Rust JS Worker、MemeLoop server bundle、模型 Provider 与 GEO 工具桥接仍未完成。
- P00 回归通过：Rust format、Clippy `-D warnings`、1 项 P00 API、14 项既有 API、1 项应用、12 项领域、3 项持久化测试；前端格式、类型、28 项测试和生产构建。2 项 PostgreSQL 条件测试仍因未配置 `GEO_TEST_DATABASE_URL` 跳过。
- 真实浏览器完成“登录 → 创建并启动项目 → 项目根重定向 P00 → 新建会话 → 提交任务 → 显示 Rust JS Runtime 未配置 → 刷新恢复消息与运行状态”验收；P00 视觉布局、首项导航和无模拟回复行为通过。
- 完成脱离历史对话的工程交接审计，新增 `HANDOFF.md`，集中说明文档权威顺序、产品主线、实现/未实现边界、运行模式、验证命令、关键代码入口和 W00 下一任务；README 与精简待办只增加交接入口，冻结产品规格未改动。
- 根据产品负责人明确决定，将社区核心开源协议从 AGPL-3.0-only 切换为 Apache-2.0；同步更新根许可证、Rust/Node 元数据、README、贡献说明与产品规格中的许可边界，OEM 品牌和托管服务继续由独立商业协议约定。

## 2026-09-21

- 环境修复：本机 rustc 停留在 1.88.0（项目要求 1.96），且 `rustup update stable` 在 Windows 上因 `rust-docs` 组件重命名失败，把工具链改坏到 `rustc.exe` 无法加载。改用 `rustup toolchain install stable --profile minimal` 修复，现为 rustc 1.98.1。另外 `pnpm` 不在本机 PATH，改用 `corepack pnpm`（项目经 `packageManager` 锁定 10.17.1）。
- W00 兼容探针第一步完成：新增隔离 crate `crates/worker`（包名 `geo-worker`），基于 `deno_core 0.412.0` 内嵌 V8；**尚未**接入 `geo-api`/`geo-app`，符合交接说明「隔离探针」的要求。
- 7 项探针测试全部通过（`cargo test -p geo-worker`，0.33s）：ESM 跨模块加载且未授权模块被拒绝、Promise 与顶层 await、host op 双向调用与 Rust 侧状态、墙钟超时、外部取消后 isolate 可复用、堆上限下的可恢复终止、checkpoint 序列化往返。
- 关键实证（直接读 `node_modules/.pnpm/memeloop@0.3.3/.../dist`，非网页调研）：`memeloop/loop-api` 的传递闭包为 18 个本地 chunk，**不含任何 `node:` 内建导入**；其全部外部依赖是 4 个纯 JS npm 包 `zod`、`acorn`、`json5`、`semver`，四者均已存在于本地 `node_modules/.pnpm`。
- **更正一处我自己的分析错误**：同日早前记录称 `loop-api` 经 `chunk-UHPXDAHS.js` 传导依赖 8 个 `node:` 内建，并据此把工作拆成「需补 Rust host shim」的真实 bundle 腿，进而写进了 `TODO.md` 与 `HANDOFF.md`。该结论错误，已同步更正那两份文件。复核方式：`grep -rnE "(^|[^a-zA-Z0-9_.])(import|require)[[:space:]]*\(?[[:space:]]*[\"']node:" dist --include=*.js --include=*.cjs` 命中 **0** 条；全部 `node:` 字面量都位于 `TRUST_PROFILES` 表的 `forbiddenImports` 拒绝清单（`dist/chunk-UHPXDAHS.js:1031,1040`、`dist/chunk-7UJN6GWF.js:4691,4700`）、`.replace(/^node:/, "")` 正则、`spec !== "node:events"` 例外比较（`chunk-UHPXDAHS.js:1284`）以及堆栈帧正则中。那是**禁止**租户脚本导入这些内建的脚本准入策略，与依赖关系恰好相反。教训与此前那条网页调研更正一致：以本地产物为准，且不能把字符串字面量当作导入语句。
- 真实 bundle 腿的实际剩余工作是**依赖供给而非能力 shim**：把上述 4 个包经既有 `InMemoryModuleLoader` 注入即可，无需改动 `crates/worker/src/loader.rs`——该加载器已按 specifier 原样解析注入项并拒绝其余。
- 入口应走 `createAgentToolLoopRunner`（`dist/loopAPI/agent-tool-loop/directRunner.d.ts`；上游注释明确其为「Direct agent/tool runner with no dynamic script-loader dependency，React Native 使用的可移植路径」），而非会引入脚本加载器、registry 与 profile 的完整入口。
- 需中和的动态导入有两处，且都不在模块求值期触发，因此不会威胁 `crates/worker/src/bundle.rs` 的顶层 await：`dist/chunk-55H42F5Y.js:366` 的惰性 `await import("ai")`（注入自有 `ILLMProvider` 即可绕开）与 `chunk-UHPXDAHS.js:2202` 的 `import(specifier)` 逃生口（默认即 fail-closed，且可经 `AgentLoopScriptPolicy.importModule` 覆写）。
- `pnpm approve-builds` 为无效线索：`memeloop` 的 `scripts` 中不存在 `preinstall`/`postinstall`/`prepare`，`dist/` 产物已由 tsup 预构建随包发布。
- 另需注意 `dist/` 下 19 个子目录只含 `.d.ts`、无运行期 `.js`；`loop-api.d.ts` 中指向 `./loopAPI/agent-tool-loop/index.js` 的再导出只是类型解析 shim，不能据此构建模块 allow-list。
- 上游已内建与产品要求同向的隔离概念：脚本准入按 trust class（`restricted`/`quarantine`）限制 `maxScriptBytes`、`allowedInterfaces` 与 `forbiddenImports`，可作为 Rust 侧 host op 面与租户脚本策略的对照参考。
- 引擎回退路径已明确且不违反「不要同时维护两套生产运行时」：`deno_core 0.412.0` 自带 `quickjs` feature（`quickjs = [v8/quickjs, serde_v8/quickjs]`），回退是同一 API 的 feature flag，而非第二套运行时。
- 更正一处调研偏差：一份网页调研报告称 `RuntimeOptions` 有 `heap_limits` 字段、`deno_core` 再导出 `deno_error`、`extension!` 生成 `init_ops_and_esm()`。逐条对照本地 0.412.0 源码后确认三者均不成立（实际为 `create_params: Option<v8::CreateParams>`、无 `deno_error` 再导出、生成 `init()`）。实现以本地源码为准，不采用该报告结论。
- 隔离边界已落地：JS 只能经 4 个窄 host op 触达 Rust（模型补全桩、事件发射、调用计数、checkpoint）；模块仅限 Rust 显式注入的内存 allow-list，无文件系统、网络或包 registry 解析；checkpoint 采用 Rust 拥有的宿主状态序列化，而非 V8 堆快照。
- 后端与前端已在内存开发模式下启动并完成端到端验证：`/health/live`、`/health/ready`、`/api/v1/auth/config` 与登录（含 Origin/CSRF 校验）经 Vite 代理 `127.0.0.1:5173 → 127.0.0.1:8080` 全部通过。
- 新增工作区依赖 `deno_core = "0.412.0"`、`deno_error = "=0.7.1"`。注意：引入 V8 会显著抬高 `cargo test --workspace`、`cargo clippy --workspace` 与 CI 的构建时间，需要在 CI 中显式安排。
- W00 安全 Host Ops 完成：`crates/worker/src/host.rs` 定义封闭且带版本的 op 面（`geo.hostops.v1`，5 项：`model.complete.v1`/`knowledge.search.v1`/`manifest.read.v1`/`publish.submit.v1`/`measure.sample.v1`）与 `HostOps` trait 边界，实现在 `crates/api/src/agent_runtime.rs`——依赖方向是 `geo-api → geo-worker`，worker 不反向依赖 API，隔离边界是真实接缝而非命名约定。
- 边界要点：不在 `HostOp` 枚举里的能力没有 op、也就没有 Rust 实现体；请求 DTO 全部 `#[serde(deny_unknown_fields)]` 且不携带 tenant/project 选择器，作用域只能来自 Rust 侧 `HostBridge::scope()`；预算、单次调用截止与取消集中在 `HostBridge::invoke` 用 `tokio::select!` 统一施加，不在各 op 重复实现；凭据在 `HostOpError::failed`/`internal` 与 JS 边界两处脱敏。
- `HOST_OP_ERROR_BOOTSTRAP` 在任何 bundle 模块之前执行 `registerErrorClass`。不注册时 `deno_core` 会退化成无关的 `TypeError: invalid_argument`，导致所有失败路径误报——这是本任务中实际发现并修复的问题。
- `crates/app/src/main.rs` 每次装配一个运行时装在两条存储分支之前。当前装配 `unconfigured()`，因此每个被接受的 run 都以 `capability_missing` 如实失败；`RepositoryHostOps` 只实现 `knowledge_search`，其余 4 项返回带具体原因的 `capability_missing`，**没有用空结果冒充完整结果**。
- W00 Agent PostgreSQL 持久化完成：新增 `migrations/0007_agent_state.sql`（305 行，9 张表），`PgAgentRepository` 由 93 行 fail-closed 桩实现为完整实现，**未添加内存回退**，`DependencyUnavailable` 映射保持不变。
- 并发正确性的关键设计：会话行 `SELECT … FOR UPDATE` 作为幂等重放、单活跃 turn、消息序号分配与事件游标的串行点；`cancel_turn` 改为锁 **run** 行，使取消与完成成为一次串行判定；会话内事件序号在锁内以 `COALESCE(MAX(sequence),0)+1` 分配，并以 `UNIQUE` 约束兜底。
- **首次真实执行 PostgreSQL 条件测试**（此前自 2026-09-18 因从未配置 `GEO_TEST_DATABASE_URL` 而一直跳过）：用本机一次性 `postgres:16-alpine` 容器跑 `cargo test -p geo-persistence --test postgres -- --ignored`，**12 passed / 0 failed**，容器随即销毁。覆盖租户与项目隔离、幂等提交与同键异请求冲突、Run 状态机、checkpoint 重启后往返与输入变更冲突、ToolCallLedger 幂等追加、5 个并发写入者下事件序号单调无缺口、重启后重放、取消与完成竞争、仅附件消息与前序 turn 关联。
- 另有一项**不**加 `#[ignore]` 的 fail-closed 测试：指向 `127.0.0.1:1` 且 250ms 获取超时，无需凭据即可常驻运行，证明数据库不可达时列表/创建/重放均返回 `DependencyUnavailable`。
- 真实 bundle 腿可行性已实证（结论见上文更正条目）：以 esbuild 0.25.12 打包 `memeloop/loop-api` 得到 1,528,179 字节的自包含 ESM，残留裸 specifier 0 个、`node:` 引用 0 个、`process.`/`Buffer`/`require(` 引用 0 个，在 Node 中求值成功且导出 `createAgentToolLoopRunner`。配方要点：仅需 `--external:ai`（其 `node:fs/os/path` 来自传递依赖 `@vercel/oidc` 而非 memeloop 自身，且只出现在我们本就要替换的惰性 provider 路径上，esbuild 已直接从入口 tree-shake），并需显式 `--main-fields=module,main`（neutral 平台默认不读这些字段，否则 `json5` 解析失败）。**该 1.5 MB 产物未提交进仓库**——它内含 zod/acorn/json5/semver 第三方代码，合并入本仓库需相应保留许可声明，属需产品侧定夺的决策。
- 环境结论（前端 `format:check`）：本机 `core.autocrlf=true` 且仓库无 `.gitattributes`，git 检出将文件转为 CRLF 而 prettier 默认要求 LF，因此该 gate 在本机对**任何**被检出的文件都不可能通过。证明方式：加 `--end-of-line crlf` 后 prettier 对 `apps/web` 全部 41 个文件报告 "All matched files use Prettier code style!"，即 47 个报错文件中无一存在真实风格问题。Linux CI 检出为 LF，该 gate 在 CI 正常。因此这是本机环境限制而非代码缺陷。
- 至此本轮验证：`cargo fmt --all -- --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace --all-targets` 均通过（74 passed / 12 ignored，后者已单独实库验证）；前端 `typecheck`、28 项测试、生产构建通过。
- 未完成：run executor（有桥但无进程驱动一次真实 turn——配置运行时时 run 会停在 `queued`）、模型 Provider 与 Token Center 真实调用、GEO 工具桥接实现、真实 bundle 在 `crates/worker` 中的加载落地、首个完整纵切。

## 2026-09-23

- 修复用户报告的 P01「发布资源与预算」估算面板缺陷：`costs.phase_two_distribution` 显示为"待能力快照"，而同组成本行显示"待分项估算"、面板自身的 blockers 又写着 `pricing_snapshot_unavailable`（scope=`costs`），三处结论互相矛盾。
- 根因是 `unknownEstimateLabel` 用**子串嗅探英文散文**推断标签，且判定顺序决定胜负：`phase_two_distribution` 的 reason 为 `"PricingSnapshot, CapabilitySnapshot, and distribution denominator are unavailable."`，同时含 pricing 与 capability，因 `capability` 分支先匹配而误判为能力缺口。`costs.measurement` 同病因把成本行显示成"待测量协议"。
- 改为按 blockers 的**结构化** `scope` 定位：`documents` / `document_platform_targets` / `measurement_samples` / `costs` 四个 scope 与七行估算一一对应，标签由 blocker 的稳定 `code` 映射得到，不再解析任何散文。同时删除了兜底的"待知识规划"——它对一个未知原因断言了具体成因，现改为如实显示"待确认"。
- 展示缺口一并修复：`EstimateItem` 原先仅在 `state !== "unknown"` 时渲染 `<small>{reason}</small>`，而当前七行全部是 unknown，导致**真实原因从不显示**。现在未知行显示对应 blocker 的中文说明（复用既有 `estimateBlockerText`），用户能看到究竟缺哪个快照。
- 样式失配修复：`.estimate-total` 的 `grid-column: 1 / -1` 在 `styles.css` 有两处声明但标记中从未应用该类，已挂到"预计总成本"行；`.estimate-coverage` 由 `repeat(4, …)` 改为 `repeat(3, …)`，此前第四个空列无对应子项。
- **测试此前无法发现该缺陷**：`SetupPage.test.tsx` 的夹具把**已翻译的中文标签**直接写入 `reason` 字段（如 `reason: "待能力快照"`），而 `unknownEstimateLabel` 正是靠 `includes("能力")` 匹配，于是 reason 被映射为自身，断言 `getAllByText("待能力快照").length > 0` 恒真；夹具还断言了真实服务端**不会产生**的标签（`phase_one_documents` 与 `total` 两行）。已用真实英文 reason 串与四个真实 blocker 重写夹具。
- 先证伪再修：改写后的测试在原实现上**确实失败**，报错正是用户报告的那一行（`expected '待能力快照' to be '待分项估算'` at 第二阶段分发成本），修复后七行标签、原因展示、`estimate-total` 类名全部通过。
- 夹具不是凭记忆重建：以真实 HTTP 调用核对过服务端载荷（`POST /api/v1/projects/estimate`，200），逐条比对七个 `reason`、四个 blocker 的 `code`/`scope`、两条 `assumptions` 与 `estimator_version`，与夹具完全一致。
- 本轮验证：`pnpm typecheck` 通过；`pnpm test` 28/28 通过（`SetupPage.test.tsx` 10/10）。后端 `/health/live`、`/health/ready` 与前端 `127.0.0.1:5173` 均 200。
- 已知限制：夹具仍重复了服务端的英文 reason 原文，若上游改词会漂移；但修复后标签已不依赖该文本（仅无匹配 blocker 时的兜底路径会显示原文），故漂移不再影响标签正确性。
- **W00 run executor 完成**：进程内 `spawn_blocking` 执行器，无 NATS/Redis、无轮询 worker。`append_message` 提交**之后**（即事务已提交、绝不在事务内）由 handler 调用 `run_executor::dispatch`：`begin_run`（原子 `UPDATE … WHERE status='queued' RETURNING`，`None` 即静默返回）→ `run_turn` → `finish_run`（**单事务**写 run 状态、错误、assistant 消息、turn 终态与事件）。`begin_run` 的原子转移而非 handler 的状态判断才是正确性守卫——重放的 Idempotency-Key 返回的是**存储的受理回执**，其 `run.status` 仍读作 `queued`，哪怕 run 早已结束。HTTP 响应只陈述**受理**，永不乐观地写成 `running`。
- **本轮最重要发现：多线程运行时下隔离体会中止整个进程——这是真实生产缺陷，不是测试假象。** `deno_core` 的 op driver 对首次轮询未完成的 op future 调用 `deno_unsync::tokio::spawn`，而该函数 `debug_assert!(Handle::current().runtime_flavor() == CurrentThread)`，并在**该断言成立**的前提下把非 `Send` future 伪装为 `Send`。`crates/app/src/main.rs` 用的是 `#[tokio::main]`（多线程），于是任何一次真正 yield 的 op 在 debug 下触发 `STATUS_STACK_BUFFER_OVERRUN` 中止进程，在 release 下没有断言则把持有 `Rc<RefCell<OpState>>`/V8 句柄的 future 交给别的 worker 线程执行——未定义行为。修复分两半，**缺一不可**：隔离体在自己的 **current-thread** 运行时上构建、驱动并销毁（且必须在同一个 `spawn_blocking` 任务内，因为隔离体非 `Send`）；能力调用（`HostBridge::invoke`）改为投递回**应用运行时**执行，因为 tokio I/O 资源绑定创建它的运行时，连接池不能跨 turn 迁移。证伪方式：把 executor 换回隔离体自身的句柄，测试即以 `saw ["current_thread", "current_thread"]` 失败。
- 新增测试（worker）：`call_main` 的 6 项——`calling_main_runs_one_turn_under_the_run_scope`、`evaluating_the_entry_module_does_not_run_a_turn`（钉住"求值不等于运行一次 turn"）、`a_bundle_without_main_is_refused`、`a_main_that_is_not_a_function_is_refused`、`a_main_that_throws_fails_the_call`、`a_main_awaiting_an_unresolvable_promise_fails_promptly`（立即失败，而非拖到 deadline）、`an_unbounded_turn_is_terminated_at_the_deadline`。三项：反伪造测试 `an_available_runtime_that_cannot_answer_records_no_message`（桥无法作答时 run 落 `failed`/`capability_missing` 且**零**条 assistant 消息）、取消压过完成的 `cancelling_an_executing_turn_outranks_its_completion`，以及 `a_configured_assembly_runs_the_turn_to_a_recorded_answer`。
- 新增测试（PostgreSQL）：`agent_begin_run_claims_a_queued_run_exactly_once`（4 个并发声明者恰好一个 `Some`；再次声明与兄弟会话均 `None`；已取消的 run 也 `None`；恰好一条 `run.running` 事件）与 `agent_finish_run_records_the_answer_once_and_never_over_a_cancellation`。
- **两次变异检查（先证伪）**：把 `crates/persistence/src/agent.rs` 的终态守卫 `if run.status != RunStatus::Running` 改为 `if false && …`，测试以 `a terminal run must not be finished again` 失败，还原后 14/14 通过——证明终态守卫被真实覆盖；把 `HostBridge` 的 executor 换回隔离体句柄，运行时路由测试失败——证明 flavor 投递被钉住。
- 一处测试侧真实约束（记录以免后人重踩）：turn 悬挂在**正在被销毁的运行时**上的 timer 会在该运行时内部 panic（`A Tokio 1.x context was found, but it is being shutdown`），而非在测试里失败。因此取消测试使用有界 `STALL`（500ms）并等待其结束，让 turn 在测试运行时销毁**之前**完成，使 teardown 有序。
- 本轮验证：`cargo test --workspace --all-targets` **92 passed / 14 ignored / 0 failed**；一次性 `postgres:16-alpine`（端口 55432）上 `cargo test -p geo-persistence --test postgres -- --ignored` **14 passed / 0 failed**，容器随后销毁。
- 运行中的开发服务器**端到端实测**：`POST /api/v1/agent/conversations/{id}/messages` 返回 `202` 且 `run_status: "failed"`、`error.code: "capability_missing"`；会话详情为**一条 user 消息、零条 assistant 消息**，turn 与 run 均 `failed`；SSE 回放为 `conversation.created → message.created(user) → turn.accepted → run.failed`。**没有 `run.running` 是正确的**——未配置运行时的 run 在受理时即落终态，从未被声明。成功路径（`run.running → message.created(assistant) → run.succeeded`）**无法**在这台服务器上演示：`crates/app/src/main.rs` 装配的是 `EmbeddedAgentRuntime::unconfigured()`，且**故意没有**能打开"部分配置"运行时的环境开关；该路径只由使用参考 bundle + 桩桥的 API 测试覆盖。
- **clippy 收口**：`crates/domain/src/agent.rs` 的 `collapsible_if` 已用 let-chain 修掉（`if let Some(turn) = … && turn.status == …`，语义等价）。该 warning **由本轮引入**（base 为 0 条），而 CI 对每个 PR 跑 `cargo clippy -- -D warnings`，不修则本轮全部 PR 的 CI 必然为红。`crates/api` 内的同类 warning 同轮修掉；`cargo fmt --all -- --check` 同时暴露了 `crates/persistence/tests/postgres.rs` 的 3 处换行漂移（同样是本轮引入，base 为 0），已用 `cargo fmt --all` 修正。二者现已全部通过。
- **更正本文件 2026-09-21 的本地环境结论**：该条称加 `--end-of-line crlf` 后 prettier 对 `apps/web` 41 个文件全部通过、故"47 个报错文件中无一存在真实风格问题"——**这个结论是错的**。首个 PR 的 CI 以 `prettier --check` 在 `apps/web/src/pages/SetupPage.test.tsx` 失败：两处超长 `reason` 字面量未按 prettier 期望在键后换行。CRLF 噪声恰好掩盖了这一处真实问题：本机 `format:check` 对**每一个**检出文件都报错，等于该信号已失效，不能由"本机全红"推断"没有真实缺陷"。可复现 CI 的做法是取出**提交后**的（LF）内容再检查：`git show <rev>:<path> > /tmp/x.tsx && prettier --check /tmp/x.tsx`。修复已作为独立提交进入各分支。
- 已知限制（已同步进 `TODO.md`）：**重启对账缺失**——进程内没有任何优雅关闭，退出时在飞的 run 会永久停在 `running`，且修法只在单进程假设下成立、多副本下错误；取消不触达隔离体（`cancel_turn` 语义正确，`finish_run` 不会覆盖 `Cancelled`，但 turn 仍跑到 deadline）；**无堆上限**（`start` 传 `None`，`install_heap_limit_guard` 目前是死代码）；checkpoint 与 tool-call ledger 已有持久化实现但运行路径尚未写入。

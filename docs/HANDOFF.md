# Memeloop GEO 工程交接

更新时间：2026-10-26。本文提供当前状态、运行入口及必要边界；无需聊天历史即可接手。

## 文档职责

- [product-plan-v1.md](product-plan-v1.md)：产品与验收基线，仅在产品负责人明确变更需求时修改。
- [TODO.md](TODO.md)：未完成工作、当前优先级和验收目标。
- [WORKLOG.md](WORKLOG.md)：完成结果、精确提交、检查与真实运行证据，以及失败定位过程。
- 本文：只维护一份当前检查点、代码导航和运行说明；历史检查点不继续累积。

## 当前检查点

当前优先交付测量产品体验与真实浏览器验收；发布开发与真实发布测试暂停。演示环境已按授权初始化新空库并恢复既有 bootstrap 身份，停用原卷保留；旧业务样本仅作历史证据。恢复发布测试前核对既有待执行请求，避免意外重发。

| 范围         | 当前已确认状态                                                                                                              |
| ------------ | --------------------------------------------------------------------------------------------------------------------------- |
| 主分支       | `6fcb79082fc1dd51054744e9e90fcbe397dde403`（PR #164）；来源／合并树一致，八项检查通过，合并分支已删除                       |
| API／Web     | 最近已核验部署为 PR #161；PR #164 配套镜像已构建，滚动部署及实际运行版本待确认                                              |
| 浏览器执行器 | 已升级 PR #164 并 Ready；配套 API／Web 滚动部署尚未完成，未以单组件就绪推定整体验收                                         |
| 最新真实测量 | 已捕获来源但流读取超时；`capture_stream`／`capture_unverified`，自动提取终态 `unverified`／`model_unverified`，可信分析为零 |
| 最新会话清理 | 返回 `authorization_required`，会话未删除；须独立核实原会话归属、权限与清理结果                                             |
| 既有固定报告 | 仍保留两份样本中的一份有效、一份缺测；不以最新失败替换或补写原报告                                                          |
| PDF 字体子集 | PR #164 已合并、八项检查及镜像构建通过；实际后端与外部冷／热缓存下载尚未验收                                                |

最新实际请求已越过原发送前配置失败，但捕获到来源不证明回答完整、搜索完成或提取可信；Ready 也不证明测量成功。继续核对现有来源、原平台回答与同次任务关联，不重发旧任务，不改写原失败证据。清理授权与测量解析分别验收。

本次配套修复已通过独立审查：流捕获使用任务剩余时间并预留回执时间，移除额外的 120 秒截断；完成恢复复用既有有界历史读取器，核对捕获时的精确消息库存并通过通用契约保存原始历史，原 partial 来源不变。两项修复仍待合并、部署和真实验收；尚无新的真实提问、持久恢复或成功清理结果。

此前第一平台的一次有效样本自动保存七项产品、九条引用、四项明确品牌归属及三项未知，没有明确排名；引文与原文一致，本次创建的会话自然删除。另一平台原执行发送前配置失败，无回答或来源。固定报告只保存一次，刷新及 1440／390 像素读回通过；最新失败不增加该报告的有效样本。品牌归属、直接品牌排名及旗下产品分别统计，契约见 [recommendation-measurement.md](recommendation-measurement.md)。

首页对话、详情页与 AI 工具已实测共用问题集及独立关注对象状态；任意话题测量无需先设置关注对象。逐题／逐条件推荐统计、明确排名分母、跨平台对照与中英文独立 CSV／PDF 已实现；夹具界面验收和真实供应商验收分别记录。多轮、多报告、多平台稳定性、明确排名及真实报告导出仍待扩展。

PDF 子集已接通 Node／Rust／前端，原字体字节不变。完整前端 73 文件／1,037 项通过；本地周期 PDF 为 3／14 页，全文提取及首／末页像素与基线一致。十一组实际浏览器下载使用 GET 与真实 HarfBuzz 库 POST 响应拦截，不能替代真实 Rust 后端及外部网络验收。此前完整字体部署的 90 秒下载超时保留，尚不能宣称冷启动已修复。精确提交、大小、测试失败及复验过程见 [WORKLOG.md](WORKLOG.md)。

接续顺序：恢复并核实最新来源的可信解析与会话清理；核验 #164 配套部署、真实后端和外部冷／热缓存 PDF；完成新扫码身份识别、加密保存与刷新；扩展多平台、品牌／排名及真实报告 CSV／PDF。存储复制、高可用与备份恢复另行验收，服务就绪不代替业务或故障切换通过。其余范围见 [TODO.md](TODO.md)。

## 产品与实现边界

默认入口为 AI 对话，立即显示输入框，首次发送时才创建会话。专业页面查看和微调同一持久数据；任意话题测量不依赖企业资料或优化周期。导航合并要求见 [ux-consolidation.md](ux-consolidation.md)。

产品主线为：版本化企业资料 → 冻结文档清单的第一层 fan-out → 文档 × 平台的第二层 fan-out → 发布与独立测量证据 reduce → 不可变周报和下一轮增量。MemeLoop 组织循环与工作流；Rust 负责权限、状态、预算、幂等、持久记录及外部副作用。

当前实现包含同源 Cookie／CSRF 身份底座、资料导入与版本、内容生成／修订、分发清单／outbox、账号与网络资源、独立测量及报告。TXT／Markdown／CSV、异步 PDF 与 Office 各有独立导入契约。真实发布、无人值守跨周期、生产费用／容量等仍按 TODO 验收，不能由工具注册或一次样本推定完成。

当前正文使用 schema 2。Agent 旧知识 search／ask 注册及启动依赖已移除，资料导入、编辑、存档和显式来源版本读取保留。生成、检查和修正提示均已迁入既有 MJS 工作流，通过 PR #82／#84 合并并包含在部署镜像中；双文档真实生成、检查及导出通过，自动修正继续独立验收。供应商协议适配归 MJS，答案、引用和推荐由 AI 提取；Rust 保存通用来源、状态与审计。

## 代码入口

Rust 的 API、领域及持久化业务均按 `accounts`、`agent`、`content`、`knowledge`、`measurement`、`partners`、`projects`、`publishing` 归类；从各目录的 `mod.rs` 进入，旧的根级业务文件路径已失效。

| 关注点              | 入口                                                                                                                                                                                                                       |
| ------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 应用装配／模型路由  | [app/src/main.rs](../crates/app/src/main.rs)、[api/src/lib.rs](../crates/api/src/lib.rs)、[runtime-deployment.md](runtime-deployment.md)                                                                                   |
| AI 会话与执行       | [AgentWorkbenchPage.tsx](../apps/web/src/pages/AgentWorkbenchPage.tsx)、[api/src/agent/mod.rs](../crates/api/src/agent/mod.rs)、[run_executor.rs](../crates/api/src/agent/run_executor.rs)                                 |
| JS 工具／内容工作流 | [agent-loop.mjs](../packages/agent-runtime/src/agent-loop.mjs)、[content-workflow.mjs](../packages/agent-runtime/src/content-workflow.mjs)、[worker/src/host.rs](../crates/worker/src/host.rs)                             |
| 问题集／测量／报告  | [QuestionSetsPage.tsx](../apps/web/src/pages/QuestionSetsPage.tsx)、[api/src/measurement/mod.rs](../crates/api/src/measurement/mod.rs)、[persistence/src/measurement/mod.rs](../crates/persistence/src/measurement/mod.rs) |
| 账号／远程登录      | [api/src/accounts/mod.rs](../crates/api/src/accounts/mod.rs)、[browser-runner/src/server.mjs](../packages/browser-runner/src/server.mjs)、[remote-login-runtime.md](remote-login-runtime.md)                               |
| 内容／发布          | [api/src/content/mod.rs](../crates/api/src/content/mod.rs)、[api/src/publishing/mod.rs](../crates/api/src/publishing/mod.rs)、[ContentPages.tsx](../apps/web/src/pages/ContentPages.tsx)                                   |

## 本地运行

先查看 `git status --short` 与最近提交，保留已有工作树和并发改动。源码、构建缓存、临时目录及测试产物使用指定工作卷；Windows V8 预编译库恢复及校验见 [README](../README.md#windows-v8-ci-artifact)，无需重新编译 V8。

环境要求：Node.js 22.17+、pnpm 10.17.1、Rust 1.96+；持久依赖可用 Docker Compose。内存模式适合 UI／API 开发，重启即丢失数据：

```powershell
pnpm install --frozen-lockfile
Remove-Item Env:DATABASE_URL -ErrorAction SilentlyContinue
$env:GEO_DEV_PASSWORD = "choose-a-local-password"
cargo run -p geo-app
```

另开终端执行 `pnpm --dir apps/web dev --host 127.0.0.1`，访问 `http://127.0.0.1:5173`，使用合成开发账号 `demo@localhost` 和上述开发密码。Vite 将同源 API 请求转发至回环后端。

持久模式先按 [.env.example](../.env.example) 启动专用 PostgreSQL，并在进程环境设置 `DATABASE_URL`；复制 env 文件不会自动载入 Rust 进程。启动自动迁移，连接／迁移失败直接终止，不回退内存。迁移不创建用户、Host 或成员关系，需按 [runtime-deployment.md](runtime-deployment.md) 显式 `--bootstrap`。

复用持久本地环境的监督启动、真实浏览器软件登录检查及交互窗口见 [local-workspace.md](local-workspace.md)；私有配置保存在仓库外。持久模式使用 Secure Cookie，浏览器须走配置正确的 HTTPS 入口。

### 模型与 bundle

执行 `pnpm agent:bundle` 生成 `packages/agent-runtime/dist/` 下的对话和内容两份 bundle，产物不提交；重建后重新计算实际文件 SHA-256 并更新配置。

本地对话单回合须同时设置 `GEO_AI_BASE_URL`、`GEO_AI_API_KEY`、`GEO_AI_MODEL`、`GEO_AGENT_BUNDLE_PATH`、`GEO_AGENT_BUNDLE_SHA256`。默认只允许回环内存模式；持久本地开发的固定作用域开关、正式租户 Token Center 路由及两者限制见 [runtime-deployment.md](runtime-deployment.md) 和 [token-center-integration.md](token-center-integration.md)。

内容工作流另配置 `GEO_CONTENT_BUNDLE_PATH`／`GEO_CONTENT_BUNDLE_SHA256`。部分配置、摘要错误或超过 8 MiB 的 bundle 会被拒绝；无模型能力如实返回 `capability_missing`。已封存内容可在无模型时恢复分发准备，不能据此推定可以生成新正文。

## Runtime 必须保留的约束

- V8 isolate 在同一个 `spawn_blocking` 任务内创建、驱动、销毁，并使用自己的 current-thread Tokio runtime；host 能力调用投回应用 runtime，避免非 Send V8 状态跨线程以及 I/O 资源跨 runtime。
- JS 只加载批准的内存模块并调用封闭 host ops，不持有凭据或任意文件／网络／进程权限。以 `HOST_OPS_VERSION` 和 `HostOp::COUNT` 为当前契约依据；跨包变更须同步 bundle、实际摘要及调用方。
- 每回合默认 64 MiB V8 堆上限、near-heap 终止、墙钟截止与 Rust 输出预算；取消触达 isolate 及挂起的模型调用，新回合不复用旧取消状态。生产多副本和资源容量仍需独立验收。
- HTTP 提交只表示受理；持久原子领取才开始执行。queued 扫描、内容执行租约与 Agent running 恢复是不同机制。
- `GEO_SINGLE_PROCESS_EXECUTOR` 默认关闭；仅整个数据库严格单执行进程才可启用旧 running 启动对账，滚动部署／多副本不得启用。running 多副本租约、优雅关闭和中途 checkpoint 仍未完成。
- tool-call ledger 的 intent／attempt／outcome 和完成结果存档不等于中途恢复；未知外部结果须查回，不盲目重发。详见 [agent-tool-ledger.md](agent-tool-ledger.md)。
- 缺测不算未提及，发布未知不算失败；清理权限独立于答案解析成功。恢复不改写原始来源，不给历史证据补造归属、完整性或删除权限。

## 部署与秘密

容器、镜像与清单入口见 [cluster-demo.md](cluster-demo.md)。先核对不可变镜像摘要、迁移和跨组件契约，再选择升级顺序：

- 升级前停止新受理／派发，核对活跃任务、登录引用、浏览器占用及未知回执，保留数据库、凭据与正在使用的会话；执行器就绪后再恢复派发。就绪状态不能代替业务验证。
- 增加执行器回传字段时先部署能接受该字段的 API，再切换执行器；远程登录 `desktop-readiness` 则先执行器后 API，回滚反向。通用回执及 host DTO／op 的跨包变更按对应 PR 配套发布。
- 持久登录支持空到期时间；旧 API 不能读取新会话，不可混用不兼容实例或直接回滚旧二进制。回滚需兼容读取或显式会话处理。
- API／runner 通过 `GEO_BROWSER_RUNNER_URL`／`GEO_BROWSER_RUNNER_TOKEN` 连接；64 位十六进制 `GEO_CHANNEL_SECRET_KEY` 必须跨重启保留，否则已有加密登录态不可恢复。资源管理员池作用域由 `GEO_OPERATOR_POOL_TENANT_ID` 配置。
- 远程登录使用维护中的 noVNC／VNC 桌面栈；不恢复截图轮询、坐标／按键接口或自制输入、剪贴板协议。成熟桌面输入法、断线及中文输入仍需实际验证。
- 模型传输诊断使用 `RUST_LOG=geo_api=info,geo_provider=info`，只记录固定阶段、状态、耗时、请求大小及有限错误分类；模型列表可达不证明推理成功。
- 真实地址、研究对象、客户／供应商身份、账号、凭据、私有截图及运行上下文不写入 tracked 文件、日志、测试夹具、提交或 PR。秘密仅由服务端环境／秘密管理器注入，禁止放入 `VITE_*`、JS、消息或 checkpoint。
- PostgreSQL FORCE RLS、非 bypass 角色、正式对象存储、权益／结算及容量目标仍以 TODO 验收；Compose 中存在 NATS／Redis／MinIO 不代表产品已接入。

## 验证与交付

按变更范围运行定向回归；完整基线命令为：

```powershell
pnpm format:check
pnpm typecheck
pnpm test
pnpm build
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

PostgreSQL 回归使用可丢弃空库，在环境设置 `GEO_TEST_DATABASE_URL` 后执行 `cargo test -p geo-persistence --test postgres -- --ignored`；测试会迁移并写数据，禁止指向共享或重要数据库。忽略测试必须显式运行，不把默认跳过当作通过。

生成 bundle 的无真实凭据装配验证：`cargo test -p geo-app generated_bundle_runs_one_turn_through_assembled_provider -- --ignored`。它使用测试 transport，不代表真实模型或渠道成功。

浏览器烟测入口为 `scripts/verify-web-smoke.mjs`：先编译本提交应用并设置 `GEO_SMOKE_APP_BINARY`，截图目录 `GEO_SMOKE_OUTPUT_DIR` 放仓库外。它使用真实 Rust API／Vite 和已有 Chromium，不下载浏览器；端口、临时目录及浏览器缓存可配置。内容模式 `GEO_SMOKE_CONTENT=1` 需先生成两份 bundle 及可用 Python 标准库，使用合成模型夹具，不继承工作站凭据。默认端口 8080／5173，夹具端口 18081，须避开已有服务。

烟测只清理自身启动的服务。可用 `GEO_SMOKE_API_PORT`／`GEO_SMOKE_WEB_PORT`／`GEO_SMOKE_PROVIDER_PORT` 指定端口，`GEO_SMOKE_TMP_DIR`、`PLAYWRIGHT_BROWSERS_PATH` 和 `GEO_SMOKE_PYTHON` 指定临时目录、已有浏览器及 Python。验收同时检查窄屏根节点溢出与内部裁切；合成流程不证明真实模型质量、发布、搜索或生产持久部署。

每个增量通过短 PR 交付至 main，最多两轮协调审查与指定 Kimi 审查，验证检查后 squash merge，标题保留 PR 编号；替代审查人须经产品负责人批准。跨包 API／工具／schema 改动列明适配要求。提交前检查 staged 内容与文件名是否含私有上下文；删除合并分支前核对无未合并工作并保留活跃工作树。

## 主题文档索引

- 测量与排名：[standalone-measurement.md](standalone-measurement.md)、[question-sets.md](question-sets.md)、[recommendation-measurement.md](recommendation-measurement.md)、[measurement-weekly-reports.md](measurement-weekly-reports.md)。
- 原文恢复与清理：[saved-observation-analysis.md](saved-observation-analysis.md)、[observation-completion-recovery.md](observation-completion-recovery.md)、[provider-conversation-lifecycle.md](provider-conversation-lifecycle.md)。
- 账号与网络：[operator-account-onboarding.md](operator-account-onboarding.md)、[operator-login-queue.md](operator-login-queue.md)、[account-network-bindings.md](account-network-bindings.md)、[durable-browser-ownership.md](durable-browser-ownership.md)、[browser-disposal-receipts.md](browser-disposal-receipts.md)。
- 资料导入：[w03-knowledge-contract.md](w03-knowledge-contract.md)、[csv-import.md](csv-import.md)、[pdf-import.md](pdf-import.md)、[office-import.md](office-import.md)、[p00-attachment-import-slice.md](p00-attachment-import-slice.md)。
- 内容与分发：[content-auto-repair.md](content-auto-repair.md)、[content-reuse.md](content-reuse.md)、[content-media-contract.md](content-media-contract.md)、[publication-reconciliation.md](publication-reconciliation.md)、[rich-publication-delivery.md](rich-publication-delivery.md)。
- 模型、报告与接入：[project-ai-settings.md](project-ai-settings.md)、[scoped-test-ai.md](scoped-test-ai.md)、[measurement-period-and-search.md](measurement-period-and-search.md)、[report-preview.md](report-preview.md)、[seo-geo-delivery.md](seo-geo-delivery.md)、[partner-integration.md](partner-integration.md)。

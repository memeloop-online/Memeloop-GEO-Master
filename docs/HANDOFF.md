# Memeloop GEO 工程交接说明

更新时间：2026-10-07

11:18 收口：版本 ZIP 本地整合通过，待提交后 CI。实际 Chromium 验证选定历史版本的 Markdown/HTML ZIP、精确原图/路径/正文及旧纯文本下载，桌面/窄屏已查看。前端最终 287/287、类型/格式/生产构建，Rust 首轮默认全量及最终严格 Clippy/格式通过；浏览器发现的开发解析器装配 409 已修订，并以三种配置的回归及媒体/导出 6/6、新二进制浏览器复验。媒体相关显式 PostgreSQL 8 项、既有内容/复用/分发/富文本 9 项通过。测试专用转发已关闭，数据库/持久卷和原演示保留，未清理缓存或重启登录环境。AI 媒体工具、缩略图服务、外部媒体发布和完整项目仍未完成；未验证输入法实验不入本批。

10:50 接续：图片批次已提交推送 `be65a8d`；主 CI `37561475062` 终态失败，Linux 前端 283/284（当前版本连续编辑用例 5 秒超时），依赖安装与新增实库步骤成功，Windows Rust 构建/测试成功。该提交不能称全绿，最后完整绿色仍为 `3fc025e`。工作区正在接版本 ZIP 导出：整批授权字节快照、相对媒体路径渲染、HTTP 下载及 UI 已写入，Rust 尚待编译/实库/浏览器验证；新依赖缓存准备中。保存计时回归修订及前端全量继续，不能借用上一批结果。遗留输入法实验不动，演示未重启。

10:18 本地收口：媒体绑定与 PNG/JPEG/WebP 实际字节检测、编辑器上传/插入/鉴权预览及历史只读已通过本地验证。前端最终 284/284、类型/格式/生产构建、Rust 默认全量/严格 Clippy/格式、媒体显式实库 6/6、原有内容实库 9/9、原生内容工作流 4/4，以及实际 Chromium 流程均通过；最新批次仍待提交后 CI。修复了切回当前版本后自动保存误退为历史只读的问题。ZIP 媒体导出、AI 媒体操作、缩略图服务与外部媒体发布仍未完成。新增依赖使用公开锁文件；本地模块解析验证通过，但完整冻结依赖安装仍需新 CI 验证。原演示服务/会话未重启，未验证输入法实验不提交；测试转发已关闭，数据库与持久卷保留。下一步先确认新 CI，再按媒体契约完成导出。

09:55 接续：最新已提交绿色基线为 `3fc025e` / 主 CI `37556479081`，Linux/Windows 均成功。工作区图片接入仍为未提交增量：媒体绑定、字节检测、编辑器插入/预览已接线；隔离 PostgreSQL 显式验证 6/6 通过（绑定 2、内容授权 3、持锁竞争 1）。API 首轮 2/4，已定位 APNG 元数据读取时机和统一响应缓存头差异，修订后正在复验。前端针对性验证通过，全量及实际浏览器继续；ZIP 媒体导出与外部媒体发布尚未实现。保留原演示服务和登录状态，未清理数据；输入法实验继续独立保留，不混入媒体批次。

09:17 收口：内部附件字节读取已实现并通过内存/完整性/隔离 PostgreSQL 三项显式验证，Rust 默认全量、严格 Clippy 和格式通过。上一提交 `1a34f14` / CI `37553166440` 终态失败（Windows 成功；Linux 前端 268/269）；保存用例现保留原工具栏点击，等待回焦并控制自动保存时机，最终前端 269/269、类型和格式通过，仍须提交后 CI 复验。媒体绑定、真实类型检测、图片预览和导出未接通，接缝与锁顺序见 `content-media-contract.md`。隔离测试资源复用，演示未重启，遗留输入法实验单独保留。

08:39：生成内容富文本本地整合收口，隔离 PostgreSQL 四组显式回归 9/9 与默认无模型/富文本两种 Chromium 流程均通过，含发现问题后的修复复验；完整验证记录见工作日志。此批提交后须确认新 CI，不能用 `eabe199` 的绿色结果替代。遗留输入法实验未提交，现有演示 API 尚未切换。

08:25 后整合：生成正文富文本已通过 Rust 默认全量/严格 Clippy、显式原生 bundle 13 项、前端 269 项、类型/构建/格式及最终实际浏览器。表格换行插入、保存中新输入保留、历史精确导出、正文前置与窄屏长引文均有回归；最新代码仍待提交后 CI，不替代真实模型或外部发布。媒体与富文本发布适配未完成，实施接缝见 `content-media-contract.md`。输入法实验仍单独保留，现有演示服务未升级或重启。

07:53：已重新查询确认 `eabe199` / 主 CI `37545455366` 终态成功。生成内容富文本仍为未提交整合增量：领域 134 项及严格 Clippy 已通过，API、前端、独立实库和实际浏览器仍需整批验证；输入法实验另行保留。原演示服务未重启，不能将测试代码视为当前演示已升级。

07:16：知识 Markdown 编辑批次已提交并推送 `eabe199`，主 CI `37545455366` 的 Linux/Windows 均在运行，不能沿用上一绿色基线宣称新 CI 已通过。工作区另有生成内容富文本领域和输入法实验增量，均未包含在本提交内；接手时保留并分别验证。

07:13：知识资料 Markdown 所见即所得完成本地整合验证：前端 253/253、类型/构建/格式通过，实际 Chromium 覆盖工具栏键盘导航、链接校验与修改、保存刷新和精确历史原文保留；桌面/窄屏已查看。采用官方 Tiptap Markdown/Table 扩展，未编辑时保留原始字节，不支持格式回到原文编辑。此批仍待提交后 CI，不代表生成内容富文本 schema 或真实模型编辑完成。后者实施契约见 `rich-content-contract.md`；输入法实验单独留存，未混入此批。

06:52 复核：最新已提交主 CI 为 `566aee0` / `37540198264`，终态成功。隔离测试数据库和独立浏览器 runner 均为 Ready，测试数据库持久卷 Bound；原本地前端、API、runner 和数据库监听仍在。仓库位置不变，构建产物继续写独立缓存卷，本轮未删除缓存或重启服务。知识 Markdown 编辑器和输入法实验仍有未提交改动，不包含在该绿色基线内。

06:30：独立私有测试 runner 已就绪，实际部署容器内完整合成 noVNC 测试通过（真实帧、指针、ASCII/退格、精确中文 emoji 粘贴、完成后撤销输入），未切换现有 API。已测 CI 镜像下载且全部 SHA-256 校验通过；测试 runner 目前使用相同已验证源码的临时依赖启动方式，并非该镜像部署验收。真实账号需重连；拼音组合仍未通过，相关未提交增量不可当作已验证默认。运营启动资料保存在仓库外。

06:20：优化信源分页的默认工作区测试与严格 Clippy 均通过，接口/领域和隔离实库回归已实际执行，继续提交后 CI。既存演示账号只读检查显示需要重连，当前不能直接发起真实测量；不是账号已保存就能视为可用。新增独立 runner 不会自动升级现有 API，切换须保留原数据和会话加密密钥。

06:15：外部 runner 启动器提交 `220e881` 的主 CI `37537626610` 成功；桌面镜像仍为已验证的 `cd0431a` 产物。工作区优化信源分页已通过接口/领域/隔离实库回归与全工作区严格 Clippy，默认测试继续；未提交输入法增量仍未实测通过。独立私有 runner 接入正在准备，不据此宣称当前演示已切换或真实搜索已成功。

05:55：`cd0431a` 主 CI `37535324360` 与桌面 CI `37535324304` 均成功，后者实际导出同一已测镜像及校验清单。新启动器支持成对配置 `GEO_BROWSER_RUNNER_URL`/`GEO_BROWSER_RUNNER_TOKEN` 使用独立 runner，不占用本地 runner 端口、不管理外部进程；独立复验 12 通过、1 既有权限测试跳过，真实环境尚未切换。运行方式见 `local-workspace.md`。未提交拼音修订仍待合成实机验收，不在该绿色镜像内。

05:30 核对：`11428a2` 主 CI `37531009210` 与桌面 CI `37531009062` 均成功，为最新已提交绿色基线。工作区后续 IBus 拼音接入尚未验收：合成测试发现切换后首字符丢失，预热修订后又出现输入法就绪超时，继续定位，不视为中文输入已完成。独立测试数据库 PVC 仍为 Bound；原持久本地服务和账号数据未重启或删除。

04:40 接续状态：最新完整绿色基线为 `b08d4f4` / Actions `37525672020`，Windows 与 Linux 均已确认成功，包含实际 UI、持久服务重启、原生 V8 和 PostgreSQL 验证。此前默认回环 Host 重复注册已修复。以下旧提交记录为历史切片，不是当前 CI 结论。

05:00 新状态：`a983d4c` 主 CI `37528063279` Linux/Windows 成功，独立桌面 CI 揭示合成身份检查中的截图异常。修订测试分离身份与实际帧缓冲检查，并通过真实 noVNC 点击取得窗口焦点；独立集群 Linux 实例连续三次通过指针、键盘、精确中文/emoji 粘贴、可见颜色区域、PNG 与完成后断开。该修订待新 CI，不能称整批已绿；中文拼音引擎与 Windows 本机 IME 仍未验收。

问题详情中英来源/用途/日期文案的前端全量 248/248、类型/格式/构建及实际浏览器流程通过；runner 81/81，npm 与 pnpm 冻结依赖验证通过。临时集群浏览器仅用于合成测试，没有真实账号或数据库卷挂载，不等同于可交付的私有账号入口。操作记录保存在仓库外，不把运行地址和凭据写入公开文档。

本批整合：信源到渠道建议 API/AI v14 工具、发布范围保存读回、下一周期分发配置快照，以及测量页四分区和草稿保留。前端全量 247/247、类型检查、生产构建、Rust 默认全量和最终严格 Clippy、显式原生 bundle 13/13 通过。后继配置的内存及一次性实库回归各一项通过，新增建议读取→HTTP 修改/读回→后继快照串联回归通过；实际外部账号、自动目标物化及发布/复测仍未完成。浏览器验证使用隔离端口，原持久演示后端未升级或重启。

当前增量：noVNC 客户端和同源授权网关、知识正文读写 v13 工具、知识修订事务回滚故障注入。最终前端 235 项、浏览器 runner 81 项、Rust 默认全量/严格 Clippy、显式原生 bundle 13 项、工具/网关针对性、两项知识修订实库与独立端口完整 UI 冒烟通过；新批次 CI 待验证。noVNC 真实 Linux 显示、中文输入、实际账号登录和搜索仍未完成；本地演示后端未升级，不把源码接入当作可用登录。信源推荐实施契约见 `source-channel-recommendations.md`。

当前整合：五组导航（测量与洞察第二）、独立测量与发布拆页、真实对话驱动的候选问题入口、引用信源与原始问答回溯、知识正文同来源子版本、Host 级 OEM 外观和中英国际化基础。前端最新 233/233、两组类型检查和生产构建通过；Rust 默认全量、严格 Clippy 与新 bundle 显式原生 V8 13 项检查均通过。知识/OEM 新实库回归已加入 CI，不依赖默认忽略测试。整合批次的新 CI 仍待验证，不沿用旧绿色结论。

`159bc55` 的交互桌面专用 CI `37509990301` 已通过；其主 CI Windows 成功、Linux 因文件上传前端测试超时失败。后续 CI 仅对测试 worker 使用两路并行，不限制业务并发。OEM 真实 Chromium 合成登录页检查通过品牌/标题/主题、语言选择跨刷新与 390px 布局；截图只存仓库外。当前运行后端尚未升级到本批，不能认为旧演示环境已经提供新 API。

知识编辑目前保留 Markdown 原文，同来源版本修订、AI 工具及事务故障注入已通过针对性验证，完整所见即所得和真实模型编辑仍待完成。OEM 尚缺标志上传、报告/导出品牌和全站文案国际化。信源统计仅代表当前批次的已核验引用，不能当作自动渠道推荐；真实问题预测模型、外部搜索/发布及闭环仍待验收。独立测试数据库已在隔离集群命名空间验证持久卷重启读回，私有启动/凭据资料不入库；原本地演示后端保持运行。

最新 CI 终态：`ea37a68` / Actions `37497357475` 已确认 Linux 与 Windows 均成功。下述旧基线保留为历史验证记录；工作区后续五组导航、知识文本修订、OEM 配置与远程桌面等增量尚未包含在该绿色提交中。编辑器重复自动保存修复及用户文案调整的 40 项针对性前端测试已通过，整批验证继续。最新体验要求见 `ux-consolidation.md`，知识修订契约见 `knowledge-text-revisions.md`；不得把工程进度文案放进用户界面。

最新完整绿色基线：`8170f63` / Actions `37438382741` 全绿。已核对终态及实际日志：问题集三项 PostgreSQL 回归、P13 创建/修订/历史浏览器路径、持久工作区重启和 Linux/Windows 均通过；堆独测/全串行/全并行各五轮通过。此前 V8 内部断言尚无确认根因，这次通过不代表已证明修复。本地 Docker 仍未恢复；本地登录环境现已通过原生 PostgreSQL 独立运行，Docker 不再是登录入口的前置条件，真实渠道登录与搜索仍须独立验收。

当前集成增量：Office DOCX/XLSX 结构导入、首发才创建会话、六组导航、单文档 Tiptap 内容编辑、v11 项目工具、v12 独立话题测量及网页模型发现，以及固定作用域的持久本地开发 AI。前端 209 项、类型/构建/格式、Rust 默认全量/严格 Clippy、显式 bundle 13 项、实库 58 项及真实 Java 解析 3 项通过；新整批 CI 与真实模型/搜索仍待验收。临时内存演示已停用，改为本地原生 PostgreSQL 17 持久运行；实际 Chromium 已验证数据库及应用重启后原登录 Cookie 和项目可直接读回，不重新登录。运行时二进制与构建目录分离，更新不得擅自停止真实账号运行；身份、密钥和启动文件保存在仓库外，不是公开材料。新的真实渠道登录入口已打开，但尚无真实搜索通过证据。体验合并任务见 `ux-consolidation.md`；富文本标记/表格/媒体、知识修订写接口及成熟远程桌面替换仍未完成。

跨周期正文复用已接领域/API/持久层/UI 及迁移 `0027`：保持原检查版本、独立本轮覆盖，编辑采用 copy-on-write。前端 155 项、默认全量、真实后继周期 API/原生工作流与 PostgreSQL 回归已通过首段 CI。内存项目/知识 guard 与数据库事务保护公开提交，分发物化在提交内复核来源；可证明历史分支重建和真实外部发布验收仍缺。契约见 [`content-reuse.md`](content-reuse.md)。

问题集批次已推送为 `091f754`，并在后继 `8170f63` CI 完成实库验证：契约见 [`question-sets.md`](question-sets.md)，迁移 `0028`，P13 问题集、P12 引用式测量和 P00 v10 工具已接线。前端 163 项、构建/格式、Node 编排 26 项、连接器夹具 72 项、Rust 默认全量与严格 Clippy 均通过；显式原生 bundle 6 项/渠道运行时 1 项及真实本地 P13 创建→修订→刷新→旧版本路径通过。历史 Actions `37435387407` 的 V8 probe 内部断言仍保留为未确认根因的问题。冻结评估身份不翻转，报告及 PDF/CSV 分开用途，但不代表 NextCycleAction 优化器或真实外部采样已完成。

历史绿色基线：`44f22ff` / Actions `37405615406`，Linux/Windows、前端、显式 V8、Java 21、PostgreSQL 和真实本地浏览器冒烟全部通过，包含报告只读预览、实时导入进度及原始收据核对。已读取 Linux 日志确认 `import_progress` 两项、`actual_pdf_pages_are_imported_as_searchable_source_evidence` 与 P14 临时预览/零写入浏览器检查实际通过，不是默认忽略后的通过。截图采用合成资料，不包含外部账号或真实发布/搜索验收。

该绿色基线包含：P00 正文分发工具读取跨周期原发送目标及查回摘要；网页搜索修正乱序聊天关联、无效引用回退，支持同回答多次搜索及原始字节初始化超时。CSV v2 对超长记录保存独立生成证据及精确单元格范围，完整原记录不变、普通知识检索不重复返回分片；新增 PostgreSQL 持久分片/检索回归已通过。真实登录、发布/搜索、P04/P12 实机视觉、旧 CSV 版本补建与完整项目验收仍缺。

查回与报告增量：无候选网址查回使用原账号完整列表发现唯一同标题/正文资产，无回执时核验不可变发送前版本；周报新增独立资产观察，保留原未知计数与旧快照。按原目标每批 64 项、每原尝试最新 32 项候选选择有效证据，迁移 `0025` 增加对应索引。新增数据库回归和迁移已通过上述 CI；发送因果证明、自动更正排期与真实账号验收仍缺。

本地构建使用独立构建磁盘恢复的 CI Windows/MSVC V8 150.4.0 产物并通过校验，实际链接该静态库，没有从源码编译 V8。默认忽略的数据库及生成 bundle 集成测试不计入默认测试结果；历史失败及修复过程保留在 `WORKLOG.md`，新提交不能沿用旧绿色结论。

重新生成 bundle 后，另以 `--ignored` 显式执行 CI 同款六组原生 V8 集成测试，共 12 项通过：工具循环/历史/报告 5 项、应用模型装配 1 项、附件导入 1 项、渠道工具 1 项、内容工作流 3 项、应用内容装配 1 项。使用注入模型和测试仓储，不代表真实模型、外部发布或官方搜索已经验收。

后续增量：P12 增加独立测量计划录入（不再固定提交空测量列表），冻结问题、账号、模型及观测协议并明确显示网页搜索适配未验证。浏览器未知发布可保留受校验的候选地址；迁移 `0023` 独立查回账本已接 PostgreSQL 后台扫描及只读执行，迁移 `0024` 在发送前保存原账号身份/出口的加密绑定。新路径只追加资产观察，不改写原未知结果、不重发；分页读取 API、P12 观察面板、跨周期原目标导航及 P00 来源渠道/正文分发摘要已接入，报告独立观察投影已有 CI 证据，发送因果证明仍缺。Agent 执行已改用仓储专用 `load_turn_input`：PostgreSQL 仅向应用传输最近最多 20 个成功问答对及当前附件，仍统计全部符合条件问答以提供省略数量；HTTP 历史详情读取不受此优化改变。

查回入口为 `crates/app/src/publication_lookup_dispatch.rs`、`crates/api/src/publication_lookup.rs`；前端为 `apps/web/src/pages/PublicationLookupPanel.tsx`。实施及只读 API 契约见 `publication-reconciliation.md`。不同内容执行的新资产 ID 已绑定 execution ID 与分支键，旧版本不迁移或覆写。

P00 历史绿色基线：`c307ff7` / Actions `37358200662`，包含历史省略提示。其前置 `14fdf32` / Actions `37355199295` 已验证下述回执投影及 P00 持久历史，新增历史数据库重启和全新 V8 隔离体回归均在显式 CI 步骤实际通过。真实渠道与完整项目验收仍未完成。

适用分支：`integration/w00-pr-stack`（本地集成，尚未合并到 `main`）

当前实现基线：P00 工具循环、周期冻结规划与内容第一层 fan-out、持久内容扫描、第二层分发覆盖账本、账号双来源、渠道账本、周报与周期推进；精确验证进度见 `WORKLOG.md`。分发 outbox 已接渠道任务及生成版本执行路径，真实外部发送尚未验收；不配置模型时 AI 保持未开启，权益、费用和真实部署验收尚未完成。

本文是脱离历史对话后的工程入口。接手者不需要读取 Codex、聊天记录或本地代理上下文；产品范围、当前状态、未完成任务和验证方式均以仓库内容为准。

回执投影绿色基线：`9581631`，Actions `37342680683` 全部通过；包括迁移 `0018` 发布桥接、迁移 `0019` 周期准备恢复、迁移 `0020` 连接器配置/不可变验证存储、API/账号页、发送前复核、runner provenance 和迁移 `0021` 保存结果到能力的投影。Linux PostgreSQL 步骤实际执行并通过新增回执投影回归，不仅是默认忽略测试；实施契约见 [`connector-verification.md`](connector-verification.md)。默认连接器能力仍未完成实测配置，真实外部发送与官方搜索验收缺失；不能据此宣称完整发布报告闭环。

当前回执投影批次：Rust 校验 runner 来源、尝试 ID、时间、版本和公开读回，追加宿主回执标记；PostgreSQL 迁移 `0021` 保存不可变尝试/账号/来源或生成版本引用。应用每 30 秒分页扫描已保存结果并幂等建立连接器格式能力，不另发文章；已有显式停用配置不被自动开启。`plain_text_article.v1` 是发布格式，不是文档语义类型；新清单按显式映射解析能力，旧清单不改写。内存模式没有此持久投影扫描，模拟回执、历史无来源标记回执不升级为真实验证。当前测试状态以工作日志为准。

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
- 已有基线支持 `text/plain`、`text/markdown` 与 UTF-8、逗号分隔、含表头 CSV，输入契约见 `csv-import.md`。PDF 文字层解析支持原件入队、独立 Tika/PDFBox 子进程、持久逐页进度、部分知识版本及失败页重试；P03/P04 和 P00 显式附件导入复用同一后台链路。DOCX/XLSX 本批已接独立配置的 POI 结构解析、持久单元进度、版本/重试及证据表格，真实 Rust→Java 三种格式与新增实库测试通过；新 CI 仍待验证。启动设置及边界见 [`pdf-import.md`](pdf-import.md)、[`office-import.md`](office-import.md)。不配置对应解析服务时明确不可用；网页抓取、OCR、原件高亮预览、向量检索、LLM 知识回答和结构化事实提取尚未完成。

### P00 AI 工作台

- P00 是侧边栏首项，项目根、项目切换和首次启动完成后默认进入 `/chat`。
- 局部集成 `@memeloop/react-ui` 的 `AgentChatView`，外围应用壳继续使用 Fluent UI。
- 已定义并实现内存版 Conversation、Message、Turn、Run、AttachmentReference、RuntimeCapability 和递增 ConversationEvent。
- 已有会话创建/列表/详情、消息提交、Turn 取消和 SSE 重放 API；支持幂等提交、同键异请求冲突、附件-only 消息、跨项目隔离、`after`/`Last-Event-ID` 恢复。
- 当前新增多回合历史接线：从同作用域持久消息重建当前序号之前的成功问答对，最近最多 20 对且 JSON UTF-8 不超过 128 KiB；整对省略并向模型报告省略数量，不生成虚构摘要。原生 MemeLoop 每轮独立恢复消息并分页读取，历史不授予附件导入权限。执行器使用有界 PostgreSQL 历史读取，HTTP 历史详情仍独立读取完整会话；SQL 保留完整合格回合计数以计算省略数量。新增实库回归的最终状态见工作日志。此项不等于中途 checkpoint 恢复。
- P00 已接多附件选择/拖拽/粘贴、逐项上传及失败重试；专用附件 API 核验字节与摘要，提交消息时核对作用域和已提交对象元数据。TXT/Markdown 可经模型显式导入工具形成知识版本，再检索并引用回答；上传本身不直接入库，原始大文件不塞入模型上下文。
- 真实 MemeLoop 已执行模型 → `knowledge_search` → Rust 检索 → 模型回答；JSON function-tool 协议可用，多回合历史本批接入（验证范围见上文），尚无流式工具分片或中途恢复。
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

- 封闭且带版本的 op 面（当前集成批次 `geo.hostops.v13`，38 项）：v11 增加项目读取/修订/估算/启动，v12 增加独立测量模型发现/创建/读取，v13 增加知识正文读取与同来源修订。问题发现不返回冻结评估正文；独立测量仅对可验证来自专用临时问题命令的真实结果返回受限原始回答/引用，冻结评估、旧计划和夹具不暴露此投影。部署须同步新 bundle 及摘要，当前批次新 CI 尚待验证。附件导入只接受 Rust 已绑定到当前回合的对象；业务工具只传受限资源引用，JS 无法取得 session、代理凭据、SQL、任意网络、文件、进程或环境变量。
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

### 周期自动准备与取消（当前整合批次）

- 迁移 `0019` 为当前活跃周期添加持久 bootstrap claim、重试时间与稳定错误码。配置内容 bundle 和模型后，后台每 30 秒分页发现尚无内容执行的周期；创建时再次校验项目未暂停且周期仍为当前周期。
- 原生 MemeLoop 内容工作流在封存后继续第二层清单展开，并另行遍历全部目标进行物化/恢复；只有 `distribution.prepared` 才表示本次准备完成。该事件不表示发布成功，也不表示独立测量完成。
- 已封存执行依旧绑定原周期；恢复不切到项目新周期，不重新生成正文。闭合阶段可在没有模型时恢复，共享原持久执行租约；暂时依赖缺失后退避再查，未知/已发送意图不被当作新发送。
- P00 取消保存后通知本进程运行；跨副本以作用域内单运行状态查询检查。取消触达 host bridge、挂起的模型调用和同步 JavaScript 循环，新 turn 使用独立取消状态。已发生外部副作用不能撤销；未知发送查回另由独立扫描器处理，边界见上文。
- 本批的本地、PostgreSQL CI 与提交证据以 `WORKLOG.md` 为准；不要把既有绿色基线套用于未提交修改。

### W09 报告 fan-in 首批实现

- `crates/domain/src/report.rs`：冻结分母、逐文档/发布/独立测量状态汇聚、证据归属/时间校验、缺口结论与不可变更正；缺测不算未提及，未知发布不算失败。
- `crates/persistence/src/report.rs` 与迁移 `0009_report_snapshots.sql`：租户作用域下保存快照，重复生成复用结果；更正创建关联的新版本。
- `crates/api/src/reports.rs`：列表、详情、证据与周期 reduce 服务。PostgreSQL 定时扫描到期周期，并恢复首次报告已存但后继未建的周期；两条扫描均使用游标分页。新周期创建不等于完整下一轮内容与发布执行。
- P14 已接列表、详情、证据、项目时区及 CSV；P00 注册 `report_get`/`report_reduce`，省略 ID 时在 Rust 中解析当前项目周期或最新快照。
- 应用已接已有文档规划、周期清单和渠道执行账本中的发布/测量目标；未建立计划显示不可用，缺测与未知结果保留。真实平台账号和 AI 搜索采样尚未验收，不能把夹具当成实际效果。文档状态无截止时间证明时仍保留时间依据缺口，不回填伪时间。
- 正式周报遵循冻结截止；独立只读临时预览的 P14/API/P00 入口将证据时间截断至冻结截止，不保存正式快照或推进周期，实施契约见 [`report-preview.md`](report-preview.md)。正式快照 PDF 导出已实现，本批补用途分类标签；下一周期优化动作及真实跨平台效果报告仍未完成。准确测试和 CI 对应关系见 `WORKLOG.md`，不能沿用旧绿色基线。

## 当前账号与发布纵切

- 两类接入共用后台能力：运营人员在 `/ops/channels` 登录账号、分组并分配到项目；用户在项目 `/channels` 登录自己的账号。持久 session 在服务端加密存储，不存入前端 localStorage，不向客户返回运营池凭据。只需必要的首次登录/失效重连，没有逐篇发布审批。
- 新增后台 dispatcher：配置 runner 时，每 5 秒分页发现到期且从未尝试的目标；账号缺失、项目暂停、来源不可公开或 runner 不可用时延后，不消耗发布尝试。账号间并行；同一运营商账号通过持久预检租约及原子 claim 防止多进程争抢。迁移 `0013`，未知发送保留租约至到期且不会重发；自动只读查回另接独立账本，不升级原发送结果。尚无每秒百次真实发布容量验证。
- P00 已注册渠道发现、冻结计划、分页查状态及单目标执行工具，应用装配复用 HTTP 的 Rust 业务服务；正常排期由 dispatcher 执行，不要求模型逐条调用。`ChannelPlan` 不冒充完整文档×平台领域清单；第一层正文生成、正式分发清单与生成版本渠道目标已通过独立 outbox 接入。生成 bundle 测试使用注入模型，不代表真实平台发布。
- 首个发布验收使用外部创作者平台账号，不要求客户部署 CMS/站群。冻结规格未改写；当前优先级以 `TODO.md` 为准。
- 项目账号入口为 `/app/:tenantId/:projectId/channels`；运营账号池入口为 `/ops/channels`。两者共用远程网页登录及加密 session，池账号通过项目分配供客户后台使用，不向客户返回池凭据。
- 新模块为 `channels`（domain/persistence/api）、`channel_jobs`（冻结目标、发送前尝试账本及报告输入）和 `packages/browser-runner`（隔离 Chromium）。迁移为 `0010`、`0011`；提交 `3f1ddb5` 已通过 Linux/Windows、固定 Chromium 夹具及新增 PostgreSQL 回归，真实平台账号验收仍未完成。
- P12 `/publications` 已接来源版本渠道账本和独立的正文版本分发覆盖页。既有渠道计划仍从公开 TXT/Markdown 来源创建；生成版本另由新分发 outbox 物化为独立渠道目标，不占用既有测量计划。发送结果未知时不允许盲目重发，后台自动查回、本轮观察读取及跨周期原目标导航已接线；真实渠道查回验收仍缺。
- 新增周期纵切：`GET /projects/{id}/cycles/current` 与 `POST /projects/{id}/cycles` 独立于不可变首次启动记录。首次周报保存后尝试创建相邻下一自然周；数据库扫描会补建“报告已存、后继未建”的周期，暂停/归档项目不推进。P12 读取当前周期；新周期只创建未封存清单骨架，不复制旧发布任务、不假装已生成下一轮内容。本批验证结果见工作日志，迁移为 `0012`。
- 部署通过服务端 `GEO_BROWSER_RUNNER_URL` / `GEO_BROWSER_RUNNER_TOKEN` 连接 runner；`GEO_CHANNEL_SECRET_KEY` 为持久加密的 64 位十六进制密钥，重启必须保留。缺少密钥只使持久凭据操作不可用，不阻断其他页面。`GEO_OPERATOR_POOL_TENANT_ID` 指定资源管理租户，池管理还需该租户内的资源/运营管理员成员身份。
- 本地内存账号与临时加密密钥在重启后失效，不能证明持久 session 恢复。真实平台身份、编辑器、发布与公开读回尚未完成账号实测；网页 AI 搜索与引用也未实测。来源推导的选择器、模拟回执或 OAuth 登录不能证明真实渠道可用。

## 当前正文生成纵切

- `domain/content.rs` 和 `persistence/content.rs` 保存独立执行、步骤租约/尝试、证据简报、结构化正文版本、检查及交接快照（迁移 `0014`）；冻结文档规划清单不被执行进度覆写。
- `api/content.rs` 负责冻结来源范围和当前公开用途校验、生成与独立检查的单次模型变换；MemeLoop 原生 `agent-agent-loop` 在单独批准的 bundle 中分页编排。基础引用检查不是完整事实保证；产品/结构化事实提取及完整品牌策略仍缺失。
- P00 通过 `content_start`/`content_execution_read` 启动及查询；当前周期尚无封存清单时，启动会读取冻结周期配置与当前知识版本并规划封存，已有封存版本不重新规划。P08/P09 已接执行、证据、正文版本和乐观并发编辑；迁移 `0019` 已接周期自动准备。两轮自动事实修正已新增持久计数、版本绑定租约与再次独立检查，验证范围见本文开头；真实跨周期无人值守闭环仍未完成。
- PostgreSQL 内容运行新增独立调度租约与 fencing、续租/失租中止、分页扫描和失败退避（迁移 `0016`）。配置内容 bundle 与模型后后台扫描运行中的内容执行；它不是 P00 Agent 回合的 queued 恢复、checkpoint 或工具副作用对账。
- 第一层就绪正文已可冻结为单独的第二层分发执行清单（迁移 `0017`）：按已冻结文档版本与平台范围物化文档×平台覆盖、确定性渠道变体、逻辑发布意图和持久 outbox，支持分页恢复、范围/来源重新核验及 P12 查阅。迁移 `0018` 已接 outbox 消费者与现有后台扫描，报告采用正式覆盖分母；目标关联回执和公开验证报告投影已有 CI 证据，真实发布与完整自动运营仍未完成，不能宣称两层发布闭环。
- 原生 V8 与 AppState/ContentService 测试使用注入模型；新增内容恢复与分发实库回归以 `WORKLOG.md` 的 CI 记录为准，不代表真实外部模型或平台验收。

## 4. 仍未实现，禁止误判为完成

- 真实 MemeLoop bundle 单回合、附件导入/知识检索工具循环以及独立的原生第一层内容分支工作流已有嵌入式 V8 验证；内容 PostgreSQL 扫描/续租不等于 Agent 回合恢复，第二层仍须接真实发布、测量与 reduce 编排。构建入口 `pnpm agent:bundle`，产物不提交。
- **queued 恢复**：PostgreSQL 模式每 5 秒按 `(created_at, run_id)` 分页发现未开始回合，迁移 `0022` 提供部分索引；配置好的运行时通过与 HTTP 相同的 `begin_run` 原子领取，再从持久消息重建输入。扫描不携带提示词或附件，不领取 running/终态，无运行时则保持 queued。竞争、取消、重启和分页回归已有 CI 证据，不宣称已完成 running 中途恢复。
- **running 重启对账**：已接 `GEO_SINGLE_PROCESS_EXECUTOR=true` 启动扫描，只适用于整个数据库严格单执行进程，默认关闭。滚动部署、多副本不得启用；running 租约和优雅关闭仍未实现。此旧对账测试已由 CI 独立 schema 验收，不能替代新 queued 扫描验证。
- **回合进行中的实时取消**：当前整合批次已接隔离体及模型等待中断，独立回合取消状态不复用；生产多副本与在途外部结果核对仍须补验，见上方当前批次说明。
- **隔离体基础保护**：64 MiB V8 堆、near-heap 终止、独立墙钟和 Rust 输出预算均已回归通过。不设固定隔离体并发准入门槛；高吞吐调度和进程资源观测仍需真实容量验收。
- **checkpoint 与 tool-call ledger**：executor 已写完成结果存档；既有 26 项 host op 的 Rust 调用侧 intent/attempt/outcome 已通过本地及 CI，内存与 PostgreSQL 共享生命周期，应用按实际 Run 注入仓储。本批三项问题集工具沿用账本并在保存成功前核对返回结果，独立验证状态见 `WORKLOG.md`；这不是中途恢复，跨副本 running 租约、稳定重入位置和可重放结果仍缺。边界见 [`agent-tool-ledger.md`](agent-tool-ledger.md)。
- Token Center HTTP 适配及持久租户/项目路由已装配，配置撤销和凭据 generation 在调用时重新校验；正式权益/费用记账、流式模型事件及真实部署验证仍未完成。本地单模型配置保持仅限内存开发模式。
- 对话 registry 已注册渠道计划/执行、第一层内容启动/查询及第二层分发启动/读取/恢复/分页工具；后者复用 `DistributionService`，不允许模型自报正文或平台能力。周期自动贯穿两次 fan-out 的原生工作流、真实搜索测量仍待补齐，不能以工具存在代替无人值守全周期运行。
- P00 非 TXT/Markdown 解析、媒体/表格与跨回合附件使用；当前存储为内存或 PostgreSQL blob，非正式对象存储服务。回合输入可由持久 Message 重建，但这不等于完整自动恢复执行。
- 项目级知识文档清单已支持规划、封存和只读 GET，P07 显示真实覆盖项及来源依赖。内容启动在当前周期无清单时按冻结配置规划，刷新封存清单不重新规划，知识当前版本改变也不覆盖历史清单。已有独立正文生成与分发覆盖矩阵；产品级细化、分发实际执行及真实连接器/账号池/出口池验收仍待完成。
- 独立 AI 渠道采样与真实发布证据接入、丰富效果归纳及自动进入下一轮；已有报告快照/汇聚器和到期周期扫描，不代表整个周闭环完成。
- PostgreSQL 全仓库事务级 tenant scope、FORCE RLS 和非 bypass 角色验收。

PostgreSQL 模式下 `PgAgentRepository` 已实现持久化，数据库故障仍 fail closed，不能回退内存。完成结果存档不代表中途恢复或工具副作用账本。复制 `.env.example` 不会自动把变量载入 Rust 进程，PowerShell 中需要显式设置环境变量。

## 5. 本地启动

数据库首次身份初始化、持久租户模型路由和独立原生内容 bundle 的新增部署入口见 [`runtime-deployment.md`](runtime-deployment.md)。相关能力处于本批集成，精确验证状态以工作日志为准；不能沿用下面旧开发路径的测试结论替代持久部署验收。

持久本地接入辅助脚本见 [`local-workspace.md`](local-workspace.md)：监督专用 PostgreSQL、Rust API、浏览器 runner 与 Vite，私有配置必须位于仓库外。`--check` 使用真实浏览器检查首次软件登录和 Secure Cookie，`--interactive` 为用户保留可操作窗口；不添加产品认证旁路、不记录外部登录截图或凭据。当前非 Docker 单元测试已通过，实际持久启动和外部账号验收状态仍以工作日志为准。

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

1. **本批验证收口**：最新完整绿色提交见本文首段；新增代码必须重新验证，不能借用旧提交的结果。先核对浏览器搜索协议测试及真实账号验收缺口。
2. **第一层内容补全**：当前周期启动可按冻结配置规划封存，PostgreSQL 内容扫描、续租和失租中止已接；补两轮自动修正、故障注入和无需手动触发的跨周期自动运营。
3. **run executor 恢复**：queued 分页发现/原子领取和实时取消已有验证；Rust-owned tool-call ledger 本批接通（验证见工作日志），继续 running 多副本租约、中途 checkpoint、结果引用和未知调用核对。不得借用内容执行的租约宣称 Agent 回合已恢复。
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

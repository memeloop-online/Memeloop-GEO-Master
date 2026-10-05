# 工作日志

- 2026-10-06 03:20 检查点：复核 Actions `37360609900`，Windows Rust 构建与测试步骤通过；Linux 前端批量导入交互超过默认 5 秒，后续 Rust/V8/PostgreSQL 步骤未执行。测试改为聚焦后粘贴多行 URL，避免逐字符重渲染，保留真实上传请求序列、部分结果和作用域断言，不扩大超时。完整前端本地 113/113 通过，CSV 数据库验收仍待新 CI；本地剩余约 0.14 GB，未下载产物或进行 Rust 编译。产品规格未改动。

## 2026-10-06

- 04:57 前检查点：P12 已接独立测量计划录入和冻结观测协议展示，仅列可接入的就绪测量账号，并明确标注真实搜索尚未验证；主代理全量前端 119/119 与类型检查通过。浏览器发送后跳转但读回失败时保留未验证候选，Rust 核验固定公开路径、摘要、原尝试关联和时间；补齐候选观察时间通过 runner 边界，浏览器代理报告 38/38 通过。候选不写成功 URL、不升级能力、不重发。
- Agent 历史读取增量：执行器改用作用域内专用接口，PostgreSQL 以窗口统计和有界结果只返回最近成功问答及当前附件；省略数量、整对字节预算、Unicode 空白与重叠/多答案排除保持原语义。新增长历史、旧回合截止、失败回合、跨作用域、异常历史及超大最近问答实库用例。主代理 Rust 全工作区默认测试和严格 Clippy 通过；PostgreSQL 用例仍 ignored，尚未实库执行。
- 独立查回仓储与迁移 `0023` 已落盘，CI 显式 PostgreSQL 套件列表纳入 `publication_lookup`。复查继续补强晚到候选、数据库时钟租约、只追加约束和同执行/原尝试外键；尚未装配自动扫描/浏览器查回/报告投影，不能宣称闭环完成。Actions `37371043207` 仍在等待托管执行器，本批不沿用旧绿色基线。
- 05:05 前 CI 检查点：`ce93ac4` / Actions `37371043207` Windows 构建、完整默认 Rust 测试和 V8 产物上传通过；Linux 零步骤取消，注释仍为未取得托管执行器，数据库测试未执行。该次整体验证不是全绿；本批新增查回/有界历史/测量界面仍需新 CI。
- 查回复查结束：晚到候选只从同一原尝试补入一次，并可恢复原已停止的查询排期；数据库时钟核验领取有效性，执行/观察记录加只追加触发器、时间检查及同尝试复合外键。最终本地持久层默认测试通过（含新候选校验单测，数据库套件仍忽略），全工作区 Clippy 与格式通过；Node bundle 21/21 复跑通过。新增 PostgreSQL 行为用例仅编译，不计实库验收。冻结产品规格与依赖锁文件均未修改。

- 本地构建恢复：在独立磁盘隔离 Cargo 缓存和 target，使用校验 TLS 的 HTTPS 下载官方锁定依赖并核验 Cargo.lock SHA-256，未关闭证书验证、未修改锁文件。从绿色 Actions `37362556996` 恢复 Windows/MSVC V8 150.4.0 缓存，构建日志确认解压并链接下载的静态库，没有本地源码编译 V8。完整 Rust 工作区默认测试已通过；数据库用例默认 ignored，不计实库通过。链接器仍报告 LNK4098，构建成功但警告尚未处理。
- 自动修正回归校正：首次本地全量与 Actions `37368746011` Linux 均在同一测试失败，原因是读取修正版时会投影已经保存的独立检查 findings，测试却与检查前空 findings 比较。改为读取并核对唯一非阻断检查，再比较完整版本，仍断言模型仅调用四次；本地全量复跑通过，不修改生产修正行为或弱化幂等断言。生成 bundle 的显式 V8 测试、Clippy 和新 CI 验证另行记录。
- 后续核验：Actions `37368746011` Windows 同样在上述断言失败，未取得新绿色基线。本地全工作区 Clippy `-D warnings`、Rust 格式检查、重新生成 bundle 后的 Node 编排测试 21/21 通过。冻结产品规格与依赖锁文件保持不变。
- 04:39 前检查点：显式执行 CI 同款六组 `--ignored` 原生 V8 测试共 12/12 通过：`memeloop_bundle` 5、应用模型装配 1、`attachment_import_runtime` 1、`channel_tools_runtime` 1、`content_workflow` 3、应用 `content_runtime_tests` 1。覆盖跨隔离体历史、附件与引用、报告工具、渠道计划、内容分支恢复和不重复模型调用。均为注入模型/测试仓储；未执行新增 PostgreSQL 回归，也不代表真实外部账号验收。

- 04:20 前检查点：Actions `37366395806` 已结束，两个作业的注释均为 “The job was not acquired by Runner of type hosted even after multiple attempts”，步骤列表为空，不归因为代码失败。queued 恢复本地提交 `ad2a023` 后，复查改为扫描方等待原子领取、仅对领取成功项创建执行任务，避免未领取任务反复堆积；HTTP 仍异步返回，不等待模型，不添加固定运行并发门槛。新增领取完成即返回及重复领取/取消回归，格式检查通过，Rust 未验证。发现独立构建空间后尝试本地领域测试：离线缺 CSV 索引，官方在线索引 TLS 握手失败；官方 GitHub 索引和 TLS 1.2 检查同样失败，未关闭证书验证、未进入编译。

- 04:05 前检查点：`93dedfa` / Actions `37366395806` 仍处于 GitHub 排队状态，无 Rust/数据库结果。另接 queued Agent 回合恢复：可信 PostgreSQL 游标扫描只返回作用域/运行 ID/创建时间，迁移 `0022` 增加部分索引；HTTP 与后台共用原子领取，并从持久历史/附件重建输入。运行时配置完成后才启动扫描，无可用运行时不领取，running 回合不重跑。新增内存执行器恢复/竞争/取消与 PostgreSQL 跨作用域分页/重启/领取竞争回归，尚未执行 Rust 测试。保留单进程 running 对账默认关闭；未知发布自动查回另记实施设计，未冒充完成。

- 同批发布查回：账号归属检查由首 20 篇扩展为固定偏移、最多 10 页，逐页核对账号，忽略外部返回的下一页 URL。录制代理回归发现拦截器的独立请求传输绕过浏览器代理，已改为临时 Chromium 页的 CDP 请求/响应拦截：不跟随重定向、仍使用已分配出口，失败不直连。主代理复跑已安装浏览器 33/33 及格式检查通过；录制代理证实归属请求携带账号 Cookie，公开读回走同出口但不携带该 Cookie。固定 CI 浏览器与真实平台账号仍须另验。

- 03:55 提交前检查：第一层内容自动事实修正已接 `needs_repair`、持久两轮计数、版本绑定检查/修正租约、追加版本和独立复检；普通资料/检查故障阻断不自动复活。修正前后复核来源用途，补检查与并发编辑的版本竞争校验。P08 显示未完成修正并继续刷新，无人工批准操作。主代理复跑前端 114/114、类型/格式、真实生成 Node bundle 21/21、全仓 Rust 格式与差异检查通过；新增数据库和 Rust/V8 回归待 CI，未在低磁盘本机编译。标准 Node 测试脚本已纳入新编排测试，冻结规格不变。

- 03:40 前检查点：`7d0dfe7` / Actions `37362556996` 全绿，包含前端、Linux/Windows Rust、显式 V8 和 PostgreSQL。已读取 Linux 日志，确认 `csv_upload_and_attachment_import_preserve_evidence_and_reject_partial_parses` 实际执行为 `ok`，CSV 完整记录证据回归同样通过。更新交接文档中已过时的周期准备/多回合接线描述，冻结规格不变。下一批两轮自动事实修正和发布归属分页正在并行实现，尚未编译/提交，不能借用本次绿色结果。

- 03:10 前检查点：P04 增加 CSV 当前记录原值表格，保留重复/空表头、空单元格、前导零、换行、空格与公式原文；格式损坏不补造单元格，原始片段仍可查。表格可键盘聚焦并横向滚动。组件与页面集成 11 项、完整前端 113 项和类型检查通过，未进行浏览器视觉验收。CSV 后端仍由 `aa7b5e1` / Actions `37360609900` 验证中，前一导入 CI 被后续推送取消，不计通过。

- CSV 后续检查：发现内容生成既有 1,600 字符证据前缀会截断 CSV 表头/值 JSON。改为只选完整 CSV 记录，超长记录不进入生成证据，知识库原值保留；普通文本前缀策略不变。补完整记录、Unicode/转义、超长跳过后保留后续记录和文本兼容回归，Rust 格式检查通过，尚待 CI。该限制在 `csv-import.md` 明示，任意长表格分片仍未实现；`dd14f86` / Actions `37359986244` 正在验证导入批次。

- 03:00 提交前检查：CSV 解析共享代码、内存与 PostgreSQL 接线已整合，原始 BOM/CRLF 摘要和严格引号/列宽校验保持一致。已从公开 crates.io 索引核实新增 `csv`/`csv-core` 版本、依赖与校验和，仅更新对应锁条目；离线 locked 元数据检查通过，不代表编译通过。全仓 Rust 格式及差异检查通过，前端 108 项/类型检查通过；后端和新增实库用例等待 CI，未将其标记为已验收。

- 02:55 检查点：`c307ff7` / Actions `37358200662` 全绿。当前并行实现 CSV 共享解析、内存/持久导入、原始摘要和表格引用；前端共用 MIME 声明，展示逻辑记录与表头，全部前端 108 项及类型检查通过。CSV 领域和 PostgreSQL 回归已落盘但尚未运行；依赖锁文件更新正在处理网络证书失败，未做 Rust 构建。输入契约见 `csv-import.md`，不宣称 XLSX、事实提取或其他解析器已完成。

- 02:40 检查点：`14fdf32` / Actions `37355199295` 全绿，日志确认历史数据库重启回归与全新 V8 隔离体模型请求回归在显式步骤实际执行为 `ok`，不以默认 ignored 计通过。新增 P00 最近回答历史省略提示，仅接受最新助手消息中的有效正整数，旧消息/无效计数不显示；定向前端测试 8/8、类型检查通过（首次遇到 jsdom 缺失 scrollTo，补测试替身后无未处理异常）。界面增量仍待新 CI；长会话有界数据库读取、中途恢复、真实发布/搜索仍未完成。

- 多回合批次 `e412f19` / Actions `37354305378`：Linux 在新增测试的 `chunks_exact` 用法触发新版本 Clippy 警告，常规 Rust、V8 和 PostgreSQL 后续步骤未执行；现改为定长数组切片，不禁用检查。另追加两个全新 V8 隔离体恢复历史的模型请求断言，确认顺序、无重复、省略计数和无附件工具权限；尚待新 CI 执行，不能将之前 Node 16/16 视为此回归已通过。

- 多回合历史提交前检查：Rust 从持久会话重建成功问答对，序号截断、作用域核验、完整对保留、UTF-8 字节预算和省略计数已实现；当前附件仍独立绑定。MemeLoop 每次调用建立独立原生消息存储，恢复历史并按游标/revision/字节分页，不拼接为伪用户提示；历史省略明确通知模型。主代理复跑真实生成 bundle 测试 16/16，含跨工具迭代与分页边界；全仓 Rust 格式和差异检查通过。新增 Rust 领域、序列化及 PostgreSQL 重启回归尚待 CI，不能用 Node 测试证明 Rust/实库全链路通过。

- 01:55 后检查点：`ec26020` / Actions `37348258685` 全绿。确认 P00 持久会话被执行器读取后，在 `TurnInput` 处仅保留当前消息，导致每次新隔离体丢失历史上下文。正在接通仅同作用域、当前消息之前的成功问答对，以完整问答对确定性保留最近上下文；不携带历史附件对象权限、不混入失败工具记录。Rust 重建、原生 MemeLoop 消息恢复/分页并行实现，新增 PostgreSQL 重启与较早回合截断回归已落盘但未运行；本批尚未提交，不宣称多回合已交付。中途恢复和完整工具账本仍为独立待办。

- 01:20 检查点：界面提交 `1c9077c` / Actions `37346030407` 全绿。搜索候选响应收集增加可选的显式完成等待、整体截止、AbortSignal 和结束后的监听清理；新增晚到匹配事件、无关/重复事件、解析失败与挂起提交回归，本机已安装浏览器测试 26/26 通过。该收集器不推断供应商搜索协议，也不把注入流程提升为真实官方搜索；生产提交、模式控制、流式协议解析和真实账号验收仍待完成。本批尚待提交后的 CI。

- 01:00 检查点：`9581631` / Actions `37342680683` 全绿，Linux/Windows、原生 V8、前端和 PostgreSQL 通过；从日志确认 `operator_registry_is_durable_revisioned_and_immutable` 在实库步骤执行为 `ok`，迁移 `0021`、保存尝试投影、重放和撤销分配回归取得验证。另补发布格式中文展示，底层仍提交原格式键，账号页定向测试 17/17 通过；该界面增量尚待整批 CI。真实发布、登录与官方搜索仍未验收。

- 复核 `6e2009b` / Actions `37209365105`：Linux 与 Windows 均在 dispatcher 的未知执行租约回归失败。原因是把匹配执行的明确 `unknown` 也归成缺测，提前释放了账号预留。修复为匹配执行的明确未知继续保持 `Unknown` 与有界预留；身份不匹配的测量回执才归为 `Missing`，并补两类状态断言。尚待新 CI，迁移 `0021` 实库验收仍未取得，完整项目未完成。本地可用空间约 0.24 GB，继续不做 Rust 编译或产物下载。

## 2026-10-04

- `f98e2c7` 的 CI `37208394173`：前端 96 项及构建、Rust Clippy 和新增分发能力测试通过；测量负例在执行 ID 不匹配时得到 `Unknown` 而非 `Missing`，Linux 常规测试失败，后续 PostgreSQL 测试未执行。修正仅将无匹配测量样本归为缺测；发布仍保留未知以避免盲目重发。补负例名称断言，等待下一次 CI，不能把本次记为全绿。

- 22:10 提交前检查：本批回执来源、迁移 `0021`、后台分页投影及格式映射已整合，`cargo fmt --all -- --check` 和差异检查通过；补齐纳秒事件的数据库微秒比较、撤销分配后的历史证明、生成变体独立正文、原始回执来源/执行 ID/伪标记拒绝回归。分发能力测试覆盖实际服务解析与版本漂移、停用和媒体格式排除；未在本地编译，CI 尚待执行。未修改冻结规格，内存投影和真实外部验收仍缺。

- 21:30 检查点：继续整合真实回执来源、保存结果到能力注册表投影及发布格式映射。复查发现数据库微秒精度、运营池账号撤销分配后的历史证明，以及生成变体与主文档正文不必相同的边界，正在补回归。后台扫描仅消费已保存尝试，不调用外部发送；已有停用配置保留。用户清理后复查本地可用空间仍约 0.17 GB，未重试清理或进行 Rust 编译；本批尚未提交/验证，真实发布及 AI 搜索验收仍缺。

- 19:15 后检查点：`ec35476` 的 Actions `37192165963` 全绿，覆盖账号页、能力配置 API、发送前停用/类型/版本检查与追加回归。下一批开始接 Rust 可信回执来源标记和基于已保存尝试的能力投影；后台扫描只读取历史结果，不调用外部发布。当前扫描装配与投影仓储仍在并行实现，未编译或提交；本地空间约 0.17 GB，停止 Rust 构建及下载，保留 CI 验证。

- 本批提交前检查：API 已接资源管理权限下的连接器配置、项目只读摘要、运行中 runner 版本发现、冻结清单能力解析与发送前复核；显式停用同时影响源资料和生成内容，按实际文档类型检查，不改写旧清单。前端 96/96、类型/格式/构建通过；API 较早的四套定向测试 19/19 通过，随后补版本变化、类型撤销、预检期间停用和有效源资料停用回归。最新全仓常规测试在 Windows 链接时因磁盘耗尽中断，新增回归尚待 CI；最终 Rust 格式和差异检查通过。缓存清理命令亦被执行策略拒绝，未删除文件、未重试绕过。
- runner 本批执行结果新增宿主盖章 provenance 与会话创建时的版本快照，adapter 不能覆盖它们或 execution ID；注入的测试适配器/浏览器始终标记 fixture，包括超时与未知结果。子代理本机浏览器回归 24/24 通过。当前 Rust 仍未消费该 provenance 建立可信验证投影，不能将其作为实际渠道已通过的证据；完整后续契约见 `connector-verification.md`。

- 17:05 前检查点：`51033fd` 的 Actions `37188433848` 全绿，迁移 `0020` 与连接器注册表实库验证通过。当前 API/页面整合已通过全仓 Clippy、前端 96 项及生产构建，最新应用二进制构建成功；全仓常规测试进行中。尝试启动本地开发服务被执行策略拒绝，没有服务启动成功证据，也未换路径绕过，暂不请求用户登录。另已写入 `connector-verification.md`，明确下一步从持久真实发布结果自动投影能力、不新增文章或审批；runner 结果 provenance 盖章已落盘，Rust 消费与可信生产投影仍待实现。

- 16:15 检查点：runner 提交 `f720e29` 的 Actions `37187552258` 全绿。连接器能力领域/持久层与迁移 `0020` 已完成局部复验：领域 2 项、迁移元数据 1 项通过，domain/persistence all-targets Clippy 通过；新增隔离 schema 实库测试加入 CI，尚待实际执行。注册表接收可信 Rust 调用的验证记录，当前还缺从已持久真实发送尝试建立验证的生产入口，不能视为连接器已可发布。页面/API 接线另批整合，16:00 整批目标未完成；前端第一轮全量 95 项、类型/格式检查通过，随后新增权限用例仍须最终复跑。

- 当前 runner 独立检查点：主代理复跑浏览器服务测试 22/22 通过（使用本机已安装 Chrome，固定 Chromium 仍由 CI 验证）。新增服务令牌保护的版本/操作能力发现，服务端读取运行中 adapter，不依赖前端或模型自报。候选搜索事件仅内部关联 request ID，不提升为有效官方搜索观测；真实账号验收仍缺。连接器注册表与其界面暂不包含在此独立提交中。

- 15:35 后检查点：`73cc646` 的 Actions `37185851870` 全部通过，内容封存后的闭合扫描/退避保留取得 PostgreSQL 实库证据，包含迁移 `0019` 的自动周期准备与恢复。下一批 runner 能力发现和候选搜索事件匹配在本机已通过 22 项浏览器夹具测试；候选事件不输出可信官方搜索结果，真实登录与搜索仍未验证。能力注册表/API/账号页尚在并行整合，不借用上一提交的绿色结果。

- 15:20 检查点：内容实库失败定位为旧步骤占用导致的 `dispatch_retry_after` 在执行封存后仍生效。修复仅在运行中转为封存时清除旧等待，不撤销现有分发租约；幂等封存必须保留后来设置的第二阶段退避。新增验证前后数据库状态与保留退避的回归，实际实库结果仍待新 CI。连接器能力持久层、生产/API 接线和账号页展示在独立工作区批次并行开发，未列为已交付。

- `b0873ff` 已推送；Actions `37183577822` 未全绿：新增内容实库测试在封存后的闭合执行扫描断言失败，当前定位修复中；其他套件及分发报告实库回归通过。取消注册窗口补充检查的定向测试 2/2 通过。本地容器服务启动失败，未重置服务或删除数据，真实持久登录验收仍未开始。部署文档已更正无模型恢复、outbox 接线及自动启动的旧描述，冻结规格不变。

- 14:40 前最终本地收口：最终文件通过全仓 Rust 格式、all-targets Clippy `-D warnings` 与常规 all-targets 测试；默认并行链接曾报 MSVC `1102`，使用 `-j 2` 重新运行通过，此为本机构建参数而非产品运行并发门槛。显式原生内容工作流 3/3、应用装配 1/1 通过，包含 300 目标分页中断、无模型闭合执行重入、历史周期绑定和无关阻断分支。取消回归包含挂起模型、同步 JS 循环、异地持久取消及新 turn 隔离；跨副本每 500ms 只读取单 run 状态，本进程取消即时通知。迁移 `0019` 与新增 PostgreSQL 恢复断言待提交后的 CI 实库验收，真实渠道与完整优化闭环仍未完成。

- 14:10 后整合检查：`a15b6a5` 的 Actions `37181675497` 全绿，包含 Linux/Windows、前端、原生 V8 与 PostgreSQL；报告时间精度修复取得实库证据。本机可用空间约 7.6 GB，继续关闭增量编译。周期 bootstrap、闭合内容执行恢复与原生第二层准备代码已落盘；第一轮全仓格式、Clippy 和常规测试通过，Node bundle 12/12 与格式检查通过。随后取消路径由整会话高频查询改为窄状态查询，须在最终文件稳定后复验；中间编译撞到未完成 trait 实现，不计作最终验证。新增迁移 `0019` 本机尚无 PostgreSQL 实测，不宣称无人值守真实渠道闭环。

- `110981d` 的 Actions `37178229420` 未全绿：Windows、前端、Rust/V8 及其他 PostgreSQL 套件通过，新增报告公开读回实库回归得到 `Unknown` 而非 `Verified`。定位为 `TIMESTAMPTZ` 微秒与 JSON 回执纳秒的精度差导致同一事件直接相等比较失败。现按数据库微秒精度比较，测试显式注入非整微秒时间并按原始持久精度断言证据时间；定向 Clippy 与实库测试二进制编译通过，实际修复效果等待新 CI，未降低回执/归属/截止验证要求。

- 12:47 最终本地检查点：全部最终文件重新通过 Rust 格式、离线全仓 all-targets Clippy `-D warnings` 与常规全仓测试。新增 PostgreSQL 报告回归已编译，按独立 schema 隔离全局 outbox 扫描，仍待本批 CI 实际执行；没有本地数据库运行证据。P00 四项分发工具、报告证据投影、测量接收和内容重试补充回归一并提交。现有非阻断 MSVC 链接警告与前端大 chunk/Zod 注释警告保留；真实外部账号发布/搜索、自动跨周期运行、费用与容量验收仍未完成。

- 12:40 检查点（北京时间）：本批显式原生 V8 回归通过：worker 对话 bundle 4 项、内容工作流 1 项、API 附件/渠道工具各 1 项、app 对话装配/内容装配各 1 项。正式报告新增目标关联证据投影：按原始发生/接收时间查询真实意图尝试与回执，每个复用目标派生独立证据引用；未知、模拟、迟到和来源后来停用不能伪造成成功或抹去在途事实。内存分发库没有可信回执 ledger，报告仍保守不升级成功。新增 PostgreSQL 公开读回/截止/跨周期复用回归将随本批 CI 实库执行，本地无数据库不视为通过。

- 12:30 检查点（北京时间）：P00 第二层四项引用式工具、正式报告目标证据查询、独立测量接收契约正在最终整合。API 定向分发 3 项、生成发布 6 项、测量 3 项、既有报告 7 项通过；全仓离线 Clippy `-D warnings`、常规 all-targets 测试、前端 90 项/类型/格式/构建及 Node bundle 12 项通过。原生 V8 显式测试另行执行；正式报告的可信公开读回 PostgreSQL 回归正在追加，不能把被忽略实库测试计为通过，因此未按 12:30 提交该批。登录修复 `e8e2086` 的 Actions `37175578745` 已全绿。

- 12:05 检查点（北京时间）：登录过期修复 `e8e2086` 已提交并推送，前端完整测试 90/90 与格式检查通过。内容服务原有瞬时失败释放租约及标题独立检查逻辑经复核仍存在；新增“检查模型失败 → 保持草稿/不能封存 → 重试就绪”与“正文有依据但标题无依据”回归，8 项定向测试通过，尚待合入下一代码批次。第二层工具、正式报告证据、测量结果接收继续补业务集成测试，12:00 未宣称这三项完成。

- 11:55 检查点（北京时间）：远程登录会话过期后关闭接口返回 404 时，前端现在正常退出并允许重新连接；截图查询 404 后停止周期轮询，未挂载登录页的截图/身份缓存立即回收。账号页面定向测试 10/10、前端类型检查通过。登录 UI/API/runner 接线存在，但本机服务与持久数据库尚未启动，真实账号登录未验证；浏览器 AI 适配器仍返回官方搜索未验证，另行补 Rust 测量答案/引用接收链路，不冒充真实搜索已可用。

- `e8ba567` 已推送集成分支；Actions `37170397496` 的 Linux Rust/V8、前端、固定浏览器与 PostgreSQL（含迁移 `0018` 分发 outbox 桥接）及 Windows/MSVC 全部成功。下文“待本批 CI”已取得这一提交的验证，不覆盖后续工作区改动。下一检查点并行补目标关联回执的报告投影与 P00 第二层分发工具；真实发布、官方搜索、费用与容量仍未验收。

- 恢复后的完整检查点：离线 `cargo test --workspace --all-targets --quiet` 通过，`pnpm build` 通过并重新生成两个 MemeLoop bundle；此前同批 Clippy、前端类型/格式与 89 项测试已通过。默认忽略的 PostgreSQL/显式 bundle 测试不计入这次 workspace 成功；迁移 `0018` 和 outbox 实库回归仍待本批 CI。以下磁盘阻断与“仅完成格式检查”记录为此前过程，不是当前验证结论。

- 空间恢复后重新验证：前端类型/格式检查及 89/89 测试通过；Rust 全仓 all-targets Clippy `-D warnings` 通过；生成版本发布 6/6、报告 7/7 定向测试通过。修复三处可合并条件的 lint 和测试中遮蔽具体知识仓储类型的变量。测试含发送前/声明后来源撤回、伪造输入拒绝、夹具不发送、冻结正文单次执行、截止与覆盖分母；不代表真实外部账号或 PostgreSQL 实库验收。关闭本轮本地增量编译以控制缓存增长。

- 本批接线代码收口：迁移 `0018` 的 outbox/独立渠道目标原子桥接、claim/回执到分发意图的事务关联、生成版本发送前全部来源重新检查、正式覆盖矩阵报告和后续页面恢复游标。声明前撤回来源不占尝试，声明后撤回保存未发送结果；夹具不升级为真实交付。修正公开文章读回的精确域名校验，并将全局 outbox 测试放入独立 schema，避免其他套件遗留数据干扰。最终代码仅完成格式和差异检查，本地测试受磁盘耗尽阻断，提交给 CI 验证；正式报告仍缺目标关联回执投影。

- 正式分发到发布任务、发送前引用检查及报告接线继续整合。上一轮磁盘不足中止了本地全仓检查；本批新增发布测试尚未完成执行验证，后续将由 CI 补验。已明确记录整体验收逾期，不将已有分发存储、模拟发布或普通模型调用视作真实平台/官方搜索闭环。

## 2026-10-03

- 绿色基线后的接线正在工作区整合：生成发布命令到独立渠道任务的事务桥接（迁移 `0018`）、发送前冻结正文/全部引用校验、正式覆盖矩阵作为报告分母并保留独立测量，以及后续页面来源恢复的游标传递。正式报告定向测试 7 项通过，随后追加的一条只读断言未复跑；分页 UI 定向测试 19 项、类型/格式检查通过。新发布测试遇到本机构建存储耗尽，尚未取得完整本地或 CI 验证；不得把未提交接线当作绿色交付，真实发布/搜索仍未验收。

- `5dc628b` 的 Actions `37131597592` 全部成功：Linux 前端/固定 Chromium、Rust、显式原生 bundle、全部配置的 PostgreSQL 套件（含迁移 `0016` 内容恢复与 `0017` 分发存储），以及 Windows/MSVC 构建和测试。渠道扫描夹具的独立 schema 修复通过。该绿色基线不覆盖随后正在实现的 outbox 消费、生成版本发送及正式分发报告接线，也不代表真实账号/搜索/容量验收。

- 整合补充回归发现：同一发布意图从来源延后恢复时，旧 `Unknown` 会被标成 `Ready`。内存与 PostgreSQL 状态投影统一优先保留已验证/结果未知状态；恢复不创建第二条发送命令。新增先普通恢复、再未知恢复的测试已通过；这不代表外部结果查回已实现。

- 当前工作区新增内容执行跨副本恢复（迁移 `0016`）：运行中执行分页扫描、独立分发 claim/心跳续租/失租中止和延后重领，与已有步骤租约分离；当前周期未封存时从冻结设置规划并封存文档清单，已有清单不重写。属于内容工作流的恢复，不改变 P00 回合 queued/取消/checkpoint 的既有限制。
- 第二层首次建立正式分发执行清单（迁移 `0017`）：封存已检查的主正文版本与平台覆盖范围，分页物化逐格状态、确定性渠道变体、逻辑发布意图和持久命令 outbox；同一逻辑意图的延后目标可恢复，未知旧意图不换号重发。P12 可查冻结分母和逐格状态。outbox 尚无正式消费者、发送前 attempt、真实发布/公开读回、P00 工具或报告接线；现有来源版本渠道账本保持独立。
- `ef43aa2` 的 Actions `37111621707` 未全绿：渠道扫描测试的全局数据夹具受到同 job 其他套件数据影响；其余 PostgreSQL 套件（包含内容、身份初始化和模型路由）通过，不能因此称整个 CI 成功。夹具在当前工作区改用隔离 schema，须等新 CI 复验。当前工作区本地离线 Rust 格式、workspace all-targets Clippy `-D warnings` 与 all-targets 测试通过；前端格式、类型及 88/88 项测试、生产构建（重新生成双 bundle）、browser-runner 18/18（本地浏览器覆盖配置）通过。显式应用级原生内容工作流 V8 测试通过；其他被忽略的显式 bundle、新分发/恢复 PostgreSQL 实库测试、固定浏览器版本和最新代码的完整 CI 仍待复验。
- 浏览器适配器增加基于已知来源行为的登录态自检与候选回答引用结构提取，测试数据不得标为已验证搜索；没有独立确认官方搜索执行和真实引用，尚未完成真实网页渠道测量。
- `b5c6125` 已推送；Actions `37108684267` 的 Linux 前端、固定 Chromium、Rust 与显式 V8 bundle 步骤通过，实库步骤发现 `channel_dispatcher` 测试夹具的 INSERT 有四列但只有三个占位符，尚未运行到新增 bootstrap/model_routes/content 实库回归。本次修正占位符；CI 改为逐套执行全部实库测试后汇总失败，避免首个失败遮蔽后续证据。修正后的实库结果等待新 CI，不标记通过。

- 当前工作区第一层内容与生产装配本地复验：`cargo fmt --all -- --check`、离线 `cargo clippy --workspace --all-targets -- -D warnings`、离线 `cargo test --workspace --all-targets` 均通过；默认忽略的生成 bundle 与 PostgreSQL 条件测试不计入普通 workspace 结果。前端 `pnpm format:check`、`pnpm typecheck`、79/79 项测试及 `pnpm build` 通过。重新运行 `pnpm agent:bundle` 后显式执行被忽略的 worker 原生对话 bundle 4 项、内容工作流 1 项，app 装配 2 项，以及 API 附件导入和渠道工具各 1 项，均通过。Node agent-runtime 11/11、browser-runner 14/14 在本地浏览器覆盖配置下通过；固定版本 Chromium 仍待新 CI 复验。保留非阻断的 MSVC 链接警告及前端大 chunk/Zod 警告；新 CI、PostgreSQL 实库、真实模型、真实浏览器账号和渠道验收仍待取得，不能把本地结果视为完整交付。
- 上一阶段第一层生成与既有渠道发送是两条独立路径：原生内容分支可产生带证据/检查的正文版本，渠道冻结计划使用公开来源版本；随后新增的正式分发清单也尚未接到既有发送器。持久身份初始化和租户/项目模型路由已装配；使用部署者显式配置且不代表费用权益或真实部署通过。
- `c4a9947` 的 Actions `37082133868` 未通过：Linux 显式生成 bundle 回归中两项旧工具数量断言未随注册表扩展更新，后续 PostgreSQL 步骤尚未执行。断言已在当前工作区修复并通过本地显式 bundle 回归；新增原生内容工作流及应用级集成回归后，仍须等待新 CI，不把该提交标为全绿。
- 第一层内容 fan-out 正在集成：独立于冻结规划清单的执行/租约、正文与证据版本、独立检查和交接快照；原生 MemeLoop `agent-agent-loop` 分页执行；P08/P09 接入内容列表与版本编辑。另新增部署者显式数据库账号初始化、租户级模型路由及浏览器受控 channel 配置。本批仍在工作区，局部测试不代替全仓、实库或真实平台验收。
- 后继周期提交 `40b6b1d` 的 Actions `37068471764` 已成功，包含 Linux/Windows 与新增后继周期 PostgreSQL 回归；该结果不覆盖后续渠道工具/dispatcher 改动。
- 账号双来源继续接入自动执行：新增到期目标游标扫描、配置 runner 后自动启动的 5 秒调度、账号级持久预检预约及带令牌 claim（迁移 `0013`）。登录/配置未就绪保持 pending；已尝试或未知目标不盲目重发，未知执行保留预约至到期。没有新增逐条审批或全局并发门槛。
- P00 新增四项只传引用的渠道工具及真实 Rust 装配，聚合协议升级 `geo.hostops.v4`，原有八项 op 保持兼容。发现结果不向模型泄露 session、出口、账号外部身份或原始正文；计划和后台调度使用同一服务。真实生成 MemeLoop/V8 + 注入模型渠道循环测试通过，Node 测试 10/10；不是外部真实模型/发布验收。
- 本批定向 API 渠道工具 3 项、dispatcher 3 项及已有账本 1 项通过，账号租约过期与旧令牌拒绝通过；新增实库测试及生成 bundle 测试加入 CI。集成检查发现并修复重复迁移元数据，回归改为核对全部迁移；Rust 全工作区 all-targets、Clippy `-D warnings`、前端类型检查/66 项测试/生产构建及格式检查通过。渠道执行工具超时/取消保留 unknown 语义，回归通过。保留 Windows LNK4098 和前端大包警告；真实平台发布、官方搜索/引用、自动查回和容量仍未验收。
- 下一周期纵切已接通：首次报告保存后幂等建立相邻自然周，冻结前周期时区/配置；晚恢复不延长历史截止。数据库以独立游标扫描补建遗漏后继，暂停/归档不推进；首次启动记录不变，P12 改读当前周期。仅建立未封存清单，不复制历史发布，也未完成下一轮增量优化。
- 本批 Rust 全工作区 all-targets、Clippy `-D warnings`、前端测试/类型检查和生产构建通过；周期 HTTP 两项验证报告后推进、重放、原始启动不变及作用域边界，P12 九项验证跟随新周期。新 PostgreSQL 后继并发/冻结配置/恢复测试已编译，加入本批 CI，尚未取得实库结果。P00 默认 reduce 仍面向当前周期，未到截止明确失败，不能把旧报告冒充当前新结果。
- 账号与浏览器账本提交 `3f1ddb5` 已推送集成分支；Actions `37051312512` 全绿，Linux（含固定版本 Chromium 夹具与三个新增 PostgreSQL 实库测试）、Windows MSVC 均成功。未合并 main；不代表真实平台账号验收。
- P12 发布记录替换占位页：读取项目周期、选择公开 TXT/Markdown 来源版本及已分配账号、封存目标清单、执行单目标并查看尝试/原始结果；未知结果不提供盲目重发。新增页面 7 项测试通过，路由已接入；自动排期、P00 发布工具与真实账号验收仍未完成。
- 新增三项 PostgreSQL 回归（`channels` 两项、`channel_identity` 一项），覆盖账号/项目作用域、密文往返、运营池分配撤销、跨表身份去重、冻结目标及发送尝试/报告分母。已离线编译，测试库未配置时忽略；CI 已增加专用执行命令，尚未取得实库结果。
- 渠道审查修复：执行后关闭会话，runner 增加闲置回收及执行总截止；跨自有/共享池身份写入使用事务锁和交叉查重；共享账号发送占用跨项目/租户生效；网页登录身份限制可信来源。公开读回使用保留指定代理的独立未登录上下文，不能以账号内可见证明公开发布。
- 本轮本地前端 64 项测试与类型检查通过；Rust 工作区 all-targets 通过，随后定向复验账号/账本；运营池与平台目录不再错误要求客户租户选择器，真实 HTTP 回归 4 项通过。Clippy `-D warnings`、前端格式通过；runner 10 项本地夹具测试通过，使用已安装 Chromium，固定版本浏览器仍由 CI 验证。无真实外部账号发布或搜索测量验收。
- 整个计划尚未完成，不能以当前前端及夹具测试作为 10 月 3 日全链路验收。下一周期创建、自动查回、P00 发布工具、真实测量与持续自动运营仍需完成。

## 2026-10-02

- 账号双来源纵切整合中：客户账号页、远程登录页、设置页已替换对应占位入口；新增独立 `/ops/channels` 运营池页面和工作区导航入口，包含分组、登录、项目分配/撤销与共享账号标识。前端类型检查及 57 项测试通过，其中账号相关 9 项；这些使用测试 API，不代表真实平台登录通过。
- 接入渠道账本的 AppState、API 路由、迁移与报告输入；正式报告读取截止前记录，更正版本使用新的证据接收边界。Rust 账号/运营池/执行证明测试仍在进行，尚未提交或取得本批 CI 结果。浏览器夹具 CI 增加固定版本 Chromium 安装，不使用外部真实账号。
- 部署样例仅记录环境变量名称；session、代理凭据、runner token 与加密密钥均不得进入前端配置、公开文档或日志。客户自有账号与运营池共用后台执行路径，不增加逐篇人工审批；真实外部发布、网页搜索测量仍待实测。

- 补充真实 SQL 回归：按周期读取初始和非首次周期、跨租户拒绝；构造 101 个未产生报告的旧周期，验证游标仍到达后续周期且不重复。已通过本地编译，使用 PostgreSQL 微秒精度避免测试时间戳往返误差；实际执行等待后续 CI。
- 报告提交 `12b7848` 的 Actions `36897454094` 全绿：Linux（含新增 PostgreSQL 报告实库并发重放/更正测试）、Windows MSVC、生成 V8 bundle 和前端检查成功。公开集成分支已推送，未合并 main；后续增加的历史周期读取/超过一页扫描测试须独立验证，不沿用此结果。
- 报告 fan-in 首批集成：不可变快照/显式更正、三类独立覆盖、证据归属及时间验证、具体缺口结论、租户作用域 API、P14 详情/证据/CSV、P00 当前项目/最新报告工具已接通。PostgreSQL 模式按 60 秒扫描已持久化到期周期，游标分页避免失败旧项占满首页；周期专用读取不依赖首次启动记录。
- 本地 workspace all-targets、Clippy `-D warnings`、Rust/前端格式、48 项前端测试、6 项 Node 测试、生产构建通过。显式生成 bundle 验证：worker 4 项、附件应用 1 项、应用装配 1 项通过；报告 API 2 项含空 ID 默认解析与跨租户断言。新增 PostgreSQL 报告并发重放/更正与扫描测试已加入 CI，待推送后实库执行。
- 已知限制：当前应用没有真实发布和 AI 测量提供者，领域测试中的样本仅为夹具；没有时间戳证明的当前文档状态不冒充截止前记录。当前仅对已存在周期生成正式周报，未实现下一周周期创建、下一轮动作、截止前临时报告、PDF、真实渠道效果指标。浏览器实机视觉验收尚未执行。

## 2026-10-01

- 报告前端已接 scoped 列表/详情、三类独立覆盖、逐平台与测量口径分组、证据定位、项目时区显示和 CSV 下载（含公式注入转义）。本地 typecheck、48 项前端测试、6 项 Node bundle 测试及生产 build 通过；本批 Rust 汇聚和调度仍在联调，未宣称真实发布/测量闭环完成。
- `e05dca9` 的 Actions `36880484964` 已完成：Linux Rust/V8、前端与 PostgreSQL 步骤成功，Windows MSVC 成功。新增附件 Object 导入实库路径已获本批 CI 验证；报告代码另处于工作区，不能沿用该 CI 结果。
- 附件导入纵切以 `e05dca9` 推送集成分支，等待本次 Actions 验证。并行启动报告 fan-in：领域与存储、API、P14、P00 工具分别实施；只有已有持久资料进入当前应用汇聚，尚无发布/测量提供者时必须显示 unavailable，不能生成伪效果。报告当前为在建，不代表交付完成。
- 附件审查修正：消息受理阶段统一拒绝重复附件 ID，避免已入队后才被 JS 适配器拒绝，新增回归通过。专用聊天上传的 internal 是原始对象暂存默认值；知识用途在显式导入时建立，不表示把已有内部知识来源改为公开，普通知识上传对象不能走此附件导入路径。
- 附件纵切收口复验：`cargo fmt --all -- --check`、`pnpm format:check` 与生成 bundle 的应用装配测试再次通过。前端 typecheck、39 项测试、Node 5 项及生产构建通过；保留大 chunk 与 Windows LNK4098 警告。本批新增 PostgreSQL 对象导入断言仍待推送后的 CI 实库验证，不能沿用上一提交的绿色结果。
- P00 附件显式导入纵切：内存/Pg Object 导入保留原文件 ID、版本、摘要与 locator；Pg 用事务级收据键串行判定重复调用，已有对象不重复 INSERT。Rust HostBridge 保存每回合不可变附件绑定，模型只能导入已受理的对象。上传不自动建库，TXT/Markdown 成功与不支持格式失败保留逐项结果。
- `TurnInput` 增加真实 message ID 与附件引用，executor 从持久 Message/Turn/Run 重建输入；完成结果 hash 包含附件版本，不代表中途 checkpoint 恢复。新增重建测试通过。
- 真实生成 MemeLoop bundle 的 import → search → answer 应用测试通过：实际上传对象、实际知识仓储、注入测试模型，断言来源溯源与两次调用同收据。不是外部真实模型验收。Node 5 项、worker 生成 bundle 3 项、API 生成 bundle 1 项通过；本地 workspace all-targets 与 Clippy 通过，16 项 PostgreSQL 测试仍由 CI 显式执行。
- 上游 0.3.3 canonical 附件会加载整份字节，canonical user turn ID 必须等于 message ID；用元数据/工具 schema 暴露附件、使用真实 message ID 并保留 GEO turn ID 完成记录，未另写 Agent 循环。外部文案调用失败，主代理仅修正了过时附件提示。
- 本批代码提交 `791215a` 的 Actions `36771176608`：Linux、Windows 全部成功，PostgreSQL 16 项实库测试全部通过（不含 non-bypass FORCE RLS），真实 V8 工具循环/应用装配和前端检查通过，Windows artifact 已上传。工作树代码已推送集成分支，未合并 main；附件显式导入下一纵切写入独立接缝文档，不修改产品规格。
- `a24aca2` 的 Actions `36769809922`：Windows 成功；Linux 其余检查通过，PostgreSQL 15/16 通过。启动重放和重启对账回归已修复；新增附件测试发现初次结果的纳秒时间与数据库微秒时间不一致，完成操作改为返回已持久化对象，确保重放完全一致，等待复验。
- 第二批功能集成：MemeLoop 原生 function-tool 循环已接知识检索；Provider/worker/bridge 保留工具定义、assistant tool calls 和 tool-result messages，不把纯工具响应的空正文当作最终答案。Node 4 项、真实 V8 bundle 2 项、本地应用 bundle 装配 1 项通过。
- 文档 `manifest_read` 已接 scoped sealed snapshot、版本/游标分页与全量覆盖统计，规划 ID 与可空正文版本分开。4 项 API host 回归和 30 项 worker host 测试通过；对话 registry 目前仅注册知识检索，文档正文尚未生成。
- P00 多附件上传纵切完成：逐项状态与重试、附件-only 提交、已成功引用保留、专用原始字节 API、作用域/摘要/元数据核验。附件存储与知识导入分离，UI 明示模型当前未读取附件。修复 PostgreSQL 上传 LEFT JOIN 的锁定目标为 `FOR UPDATE OF session`，新增附件/普通知识上传实库回归待 CI 执行。
- Token Center HTTP 适配使用可信预置租户/主体/key 映射，先校验元数据再 copy，不自行创建 key、不缓存明文；6 项本地 HTTP 契约测试通过。正式租户配置、权益/路由、应用装配及费用仍未完成。
- 新增 PostgreSQL CI 首跑 `36760856248`：13 项通过、2 项失败。确认启动幂等重放错误读取了后来变化的清单，改为读取原始 operation acceptance；重启对账测试改用独立 UUID schema，避免扫描并行测试活动 run。保留失败证据，修复待下一次 CI 实库复验。
- 整合后 workspace Clippy `-D warnings` 与 all-targets 本地测试通过（16 项 PostgreSQL 测试仍默认忽略）；真实 bundle 和应用装配测试另行显式运行通过。Windows LNK4098 链接警告仍存在。
- 同批整合前端 format/typecheck、39 项 UI/API 测试及生产构建通过；保留大 chunk 警告。以上为自动化结果，未冒充浏览器实机交互验收。
- `d22346f` 的 Actions `36757347202` 已确认 Linux/Windows 全部成功，Linux 包含真实 bundle 与应用装配测试，Windows 成功上传 V8 artifact。本机重新检查 Docker daemon 仍不可用；CI 增加一次性 PostgreSQL 17 service，显式执行原来被忽略的 repository 实库测试，等待该新配置实际执行结果，不以配置存在当作验收通过。
- 交付本地 P00 启动装配与 P07 只读清单纵切：显式环境配置加载摘要核验后的真实 MemeLoop bundle，经 Rust Provider 调模型；P07 接规划/GET，刷新不触发写操作，viewer 可读，知识版本更新后仍展示已封存快照。仍不包含正文生成、生产租户凭据、多回合恢复或外部发布。
- 本轮基础纵切验证：前端 35 项测试、TypeScript 检查、生产 build 通过；workspace Clippy `-D warnings` 通过。此前同批改动的 workspace all-targets 测试已通过，PostgreSQL 条件测试未运行，真实 bundle 应用装配测试另行通过。后续工具协议改动需要重新回归，不能沿用这组结果。前端保留大 chunk 警告。
- 同步交接与待办中的过时装配、P07 和并发门槛描述。规格书只清理一个部署地址，改为环境变量说明，产品范围与验收不变；此清理不等于已清除 Git 历史。
- 按产品负责人要求撤销本轮自行增加的“两隔离体并发上限”、容量拒绝分支和对应 admission 测试；不再将此门禁作为功能交付前置。此前并发限制记录仅为历史，当前以此撤销记录为准。优先交付 P00 应用功能与工具闭环。
- 新增每 runtime 隔离体并发上限，默认 2，支持显式有限配置；直接 start 和后台 run 共用 admission，名额由隔离体生命周期持有而非等待它的调用方持有。4 项竞争、失败释放和取消等待回归通过，既有 runtime 13 项测试通过。此限制不等于跨实例调度、总进程内存或发布吞吐验收。
- 上游 Token Center 协议已核对并记录于 `token-center-integration.md`，明确内部服务令牌与推理 key 区别、租户/主体映射、轮换和可复制凭据边界。当前尚未实现正式租户适配器。
- 集成提交 `f484d40` 的 Actions `36748375621` 已确认 Linux 与 Windows 两个 job 全部成功，包含 Linux 真实 MemeLoop bundle/V8 探针；这是此前单平台成功后的完整 CI 复验。
- 修复 Provider 总截止时间：凭据解析与 HTTP 不再分别获得完整超时额度，共享单一绝对截止；新增两段各 150ms、总预算 250ms 的回归。`cargo test -p geo-provider` 13 项通过，Provider Clippy `-D warnings` 通过。
- 官方 API client 拒绝将 API 答案标注为 consumer-surface 观测，消费端网页/移动观测需独立连接器，防止污染测量分母。新增拒绝路径回归后 Provider 14 项测试与 Clippy 通过。
- Actions `36744486610` 的 Windows 构建、测试和 artifact 上传成功；Linux 普通检查通过，旧提交真实 bundle 探针失败。已通过 `gh run download` 下载 Windows artifact，SHA-256 校验后恢复至独立 Cargo 缓存，以 `RUSTY_V8_ARCHIVE` 显式指向恢复的 archive 执行 `cargo check -p geo-worker --locked --offline` 成功；本机使用已有 Rust crates 缓存，并非全新 Rust 环境。
- 修复嵌入式运行时 UTF-8 编解码和 AbortController 能力，真实 MemeLoop 单回合在 64 MiB V8 下通过。仅暴露所需能力，流式 TextDecoder 明确拒绝；未开放任意浏览器网络或文件 API，也不代表工具循环/多回合恢复已完成。
- 整合后 workspace all-targets 测试通过，数据库条件测试与默认忽略的 bundle 探针除外；bundle 探针另行通过。workspace Clippy `-D warnings`、Rust format 与 diff 检查通过；Windows 链接保留 LNK4098 警告。
- Provider 增加真实 HTTP transport（rustls、禁止重定向、16 MiB 流式响应上限、截止/取消和固定错误信息），12 项含本地 HTTP 服务器测试通过；API bridge 支持 Rust scope 路由和模型白名单。应用仍未装配真实租户 Token Center，不能宣称端到端真实 AI 已上线。
- SSE 增加严格 Last-Event-ID/after 解析、持久仓储定期追赶及广播缺口恢复，回放和发送前重验 session/成员关系；撤销与游标测试通过。启动对账仅在显式单进程模式运行，默认关闭；中途 checkpoint/外部副作用账本仍未完成。
- 知识规划 API 绑定 W02 冻结配置，封存项目级文档清单、稳定键和公开来源版本依赖，保留 blocked 分母并限制 10,000 分支。新增迁移 0008；实库测试尚未执行，产品/事实提取、精确局部失效、后续 revision 与 P07 界面仍缺失。
- 审核 CI archive 收集逻辑发现旧版依赖 `.rusty_v8/*.lib.gz`，而上游默认缓存名是 URL 转义路径，干净 runner 也不会自动留下原名压缩包。改为按 `Cargo.lock` 精确版本下载官方 Windows/MSVC archive、完整解压校验、生成哈希与元数据，再恢复缓存供 CI 构建；artifact 上传独立于后续测试结果，但要求下载与恢复成功。
- 本机恢复脚本 fixture 通过，覆盖 URL 缓存名、内容一致、Linux target、哈希篡改和路径穿越拒绝。GitHub 最新已完成 run 仍属于此前 main，尚无本轮 artifact；不将本机已有 archive 的成功构建算作 CI 下载链路验收。
- 在生产同样的 64 MiB V8 堆上限下执行真实 MemeLoop bundle 探针，发现 `TextEncoder is not defined`，Node smoke 无法覆盖该差异。保留失败测试作为运行时兼容缺口证据，真实 bundle 尚未跑通。
- Provider bridge、重启对账、完成结果存档等并行代码整合后，`cargo test -p geo-api --lib --quiet` 11 项通过。JS metadata 不得写入权威 ToolCallLedger；完成结果存档也不等于执行中 checkpoint 恢复。整仓与数据库验收仍需单独验证。
- CI 修复已提交 `9b0b881` 并推送集成分支，Actions run `36744486610` 已确认运行中，结果与下载验收待追踪。`cargo test -p geo-domain --lib --quiet` 24 项通过；整仓测试暂受 SSE 测试适配器中 Duration 类型不匹配阻断，不能声称整仓通过。

## 2026-09-30

- 修复 checkpoint 恢复可能将自定义限额重置为更宽默认值的问题：恢复沿用当前 Rust 策略，安装前验证每条/总字节数及事件条数；输入先受序列化大小上限限制，拒绝时保留原状态。新增回归，格式与 diff 检查通过，V8 原生测试仍待执行。
- 新增真实 `memeloop@0.3.3` 的可复现 ESM 构建入口 `pnpm agent:bundle`，依赖来自锁文件，生成输出忽略且附第三方许可说明。Node smoke 2/2 通过，确认调用上游 `createAgentToolLoopRunner` 经测试模型桥接产生完成事件。当前只验证单回合，不代表工具循环、真实模型、Rust 加载、持久恢复或 fan-out/reduce 已完成。
- 后续验证进展：本次命令临时使用公共 Cargo 镜像并保持 `--locked`，成功下载依赖；领域与持久化测试执行成功，14 项需要独立 PostgreSQL 的测试跳过，本机 Docker Engine 不可用。V8 预编译库的默认下载仍失败，正在通过官方 Release 下载，未在仓库记录任何私有下载地址。
- 领域与持久化 `clippy --all-targets -- -D warnings` 通过，`cargo fmt --all -- --check` 与 `git diff --check` 通过；不将这些检查扩称为尚未编译运行的 API/Worker 验证。
- 使用临时 rsproxy 配置且 `--offline` 复跑领域/持久化目标：全部非数据库测试通过，持久化条件测试 14 项按预期跳过，`agent_unreachable_database_fails_closed` 通过；全 workspace 仍会在解析 `geo-worker` 时受 V8 构建依赖影响。
- 补齐 `EmbeddedAgentRuntime::start` 与 `run_turn` 的 V8 堆限制和 near-heap 终止守卫，默认 64 MiB、可显式配置且拒绝低于 16 MiB 的值；新增正常执行、参数校验和带父进程截止的超内存回归，待 V8 下载完成执行。V8 堆限制不等于进程内存隔离，宿主输出与总内存限制仍未完成。
- 新增 Rust-owned `op_host_emit` 输出预算：单事件、累计 UTF-8 字节数和事件条数分别限制，拒绝不改变状态；checkpoint 保持原有持久形状，恢复后的已用输出仍计入默认限制。worker 格式检查通过，运行测试待 V8 构建可用。
- 修正交接文档中的过期基线、未接入执行器及持久化缺失描述，区分本地集成状态与公共主线，不修改产品规格。
- 本地集成分支汇总 W00 PR 栈，保留原有提交父链；尚未合并到公共主线。新增 `AGENTS.md` 明确公开仓库不得保存用户提供的私有调研对象、账号和运营上下文。
- 审查发现 `DeadlineWatchdog` 的 Tokio 定时任务与 V8 同处 current-thread 运行时，同步 JS 死循环会阻塞该定时任务；原回归采用 multi-thread，未覆盖生产执行条件。
- 将守卫改为独立 OS 线程，以 channel 唤醒和 join 清理，Drop 同样收尾。新增 current-thread 下 `main` 与模块求值死循环的子进程回归，父进程提供 10 秒截止，防止回归失败拖死测试进程。
- 本轮前端 28 项测试、类型检查、生产构建通过；Rust format 与 diff 检查通过。后端测试尚未执行：Cargo 下载未缓存的 `deno_core` 时 TLS 握手失败，关闭 HTTP multiplexing 后仍失败，离线模式也缺该依赖；不将旧 CI 结果算作新修复验证。构建保留依赖注释与大 chunk 非阻塞警告。

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
# 2026-09-30

- CI 新增 Windows/MSVC 专用 `rusty-v8-msvc-<commit>` artifact：包含精确 `.lib.gz`、SHA-256 清单、target、rustc 与 V8 版本元数据；Linux bundle 单独上传，不能充当 Windows V8 缓存。
- 新增 `scripts/fetch-v8-artifact.ps1`：校验 target 与每个 archive 的 SHA-256 后，仅复制到 `CARGO_HOME\.rusty_v8`。README 已记录 `gh run download` → 校验恢复 → `cargo check` 的本地开发流程。
- 本机已验证当前锁定 `v8 150.4.0` 的 Windows archive 已落在 `I:\cargo\.rusty_v8`（约 39 MB）；本地 Cargo 可直接复用该缓存。完整 CI artifact 下载链路待下一次 GitHub Actions 成功运行后复核。
- 加强 V8 artifact 恢复：README 现在按当前 commit 选择成功的 CI run；恢复脚本要求完整 target/V8/rustc 元数据，拒绝空清单、重复或带路径分隔符的 archive，并保留 URL 转义后的 Cargo cache 键。新增 PowerShell 回归覆盖正常恢复、Linux target、哈希篡改和路径穿越拒绝；Windows CI 在上传前从干净 Cargo home 复跑恢复脚本。当前本地 fixture 测试通过，真实 GitHub Actions 下载链路仍需成功 run 后复核。

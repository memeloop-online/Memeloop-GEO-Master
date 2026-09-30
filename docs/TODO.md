# 实施待办

只保留未完成工作和当前验收目标；完成项从本文件移除，细节写入 `WORKLOG.md`。
无历史对话的接手入口见 `HANDOFF.md`。

## 当前：W00 AI 工作台与 Agent Runtime

- [ ] 在 current-thread 运行时验证模块求值和 `main` 死循环的独立墙钟终止；实现已改为 OS 线程守卫，测试必须在 V8 构建可用后执行，不能依赖被 JS 阻塞的 Tokio 定时器。
- [ ] 增加启动期重启对账：进程内没有任何优雅关闭，退出时在飞的 run 会永久停在 `running`。需要启动扫描发现遗留 run 并落终态；该做法在单进程下成立、多副本下错误，落地时必须把这个假设显式写进代码而非留给读者推断。
- [ ] 把取消接到隔离体：`cancel_turn` 语义已正确（`finish_run` 不会覆盖 `Cancelled`），但取消不触达隔离体，turn 仍跑到 deadline 才结束。`HostBridge::with_cancellation` 已备好接口。
- [ ] 验证隔离体堆上限：`start` 与 `run_turn` 已共用默认 64 MiB V8 堆限制及 near-heap 终止守卫，正常执行和超内存子进程回归待运行；另需限制进程总内存和并发，V8 堆限制不覆盖这些资源。
- [ ] 在 V8 构建可用后运行宿主输出预算测试；`op_host_emit` 已限制单事件 64 KiB、累计 1 MiB 和 1,024 事件，恢复 checkpoint 不会提高限制。
- [ ] 驱动 checkpoint 与 tool-call ledger：两者已有持久化实现，但运行路径尚未写入。
- [ ] 将 `pnpm agent:bundle` 生成的真实 MemeLoop ESM 接入 Rust 内存加载器；单回合 Node smoke 已通过，仍需补工具循环、精确消息/附件契约、持久状态适配、内嵌 V8 测试与产物摘要验证。生成文件不入库，分发时携带第三方许可。
- [ ] 实现模型 Provider 与 Token Center 真实调用；`model_complete` 目前如实返回 `capability_missing`。
- [ ] 实现 GEO 工具桥接：`manifest_read`、`publish_submit`、`measure_sample` 目前如实返回 `capability_missing`。
- [ ] 跑通“附件入库 → 带来源回答 → 两个文档分支 fan-out → 恢复 → 结果汇总”首个纵切。

## 排队：W03 企业知识库

- [ ] 接入持久对象存储与 URL、PDF、DOCX、XLSX、CSV、OCR 解析器；补齐来源替换、重试、停用和版本生命周期。
- [ ] 建立产品/结构化事实提取、冲突判定、用途边界与可追溯证据；接入向量检索和可选 LLM 回答。
- [ ] 由 KnowledgeRelease 驱动有限、版本化的文档覆盖清单，只失效受影响分支。

## W04/W09 外部 AI 渠道与竞品验证

- [ ] 为每个 AI 供应商实现能力探针，分别验证普通模型、官方搜索、引用、流式和结构化输出；API、消费端网页和移动端观测面独立建模。
- [ ] 按 `provider + model + search_mode + surface + market + language` 冻结测量协议，保存原始答案、引用、截图/响应、request ID、用量、出口和连接器版本；降级只记为代理观测或缺测。
- [ ] 验证文章发布器的草稿、渠道变体、发布、更新、公开查回、结果未知和幂等重试；复用统一的账号、执行环境、出口 IP/IP 池和证据模型。

## W01 生产加固

- [ ] 完成所有 PostgreSQL repository 的事务级租户 scope 后启用 FORCE RLS，并使用非 bypass 角色实测。
- [ ] 将 SSE 改为持久化游标恢复，并在长连接期间重新校验会话与成员关系。

## 主架构检查点

- [ ] W03/W05 建立有限、版本化的知识文档 fan-out 清单。
- [ ] W06/W08 建立文档 × 平台 fan-out 覆盖矩阵和独立发布分支。
- [ ] W09 建立发布证据 + AI 渠道测量 reduce、不可变周报和下一轮增量。

## 待补验证

- [ ] 以非 bypass 角色实测 FORCE RLS。迁移、W02 原子启动、租户可见性、Agent 持久化与重启恢复、run 声明/完成已可用一次性 PostgreSQL 实库验证通过（14/14，见 `WORKLOG.md`）；`0004_tenant_rls.sql` 仍是安全 no-op。

## 纵切目标

- [ ] 知识 → 基线 → 策略 → 内容 → 检查 → 账号池 → WordPress/Ghost 发布 → 验证 → 复测。

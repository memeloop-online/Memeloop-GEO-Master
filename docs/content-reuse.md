# 跨周期已检查内容复用：实施契约

本文件细化冻结规格 11.7 的“未变化资产不重复发布”，不修改产品范围。本契约正在实现与验收，不代表完整交付；当前提交及验证以工作日志为准。

## 当前实施边界

领域/内存、PostgreSQL registry 与迁移、内容 API、分发固定版本解析和 P08/P09 复用编辑界面已有实现。内存模式用项目与知识校验锁保护复用、检查结果、封存及分发提交；PostgreSQL 在对应事务内复核。已补真实后继周期的 API 与原生 MemeLoop 回归，不再只测试同一 execution 重放。

分发物化已补事务内来源复核，数据库回归使用真实来源型夹具，不能以模拟标记跳过来源验证。尚须完成本批 PostgreSQL 显式回归和 CI 整体验证。旧分支未能重建完整策略与语义输入时保守标记依据不足；可证明历史分支的 sidecar 重建仍未交付。不能将这些尚未验收的边界声明为自动零重复发布已完成。

## 已确认的缺口

当前 successor 周期自动启动新的内容 execution，生成新的正文 revision。分发去重包含 revision 和 variant 身份，因此“沿用同一个 revision 的分发去重测试通过”不能证明真实自动新周期不会重复发布。

现有 `ContentItem.input_hash` 包含周期/清单分支身份，不适合作为语义复用键。分发的 `checked_revision` 还检查 asset 的 execution/item 归属；不能简单赋值旧 revision，更不能取消归属校验或将旧 revision 复制到新 execution 的新增记录集合。

## 目标与数据边界

- 每轮仍保留独立、完整的文档清单、execution、覆盖项和分发目标。
- 未变化分支引用原来通过独立检查的同一 revision，并明确保存 origin execution/item、asset、revision、check、fingerprint 和复用时间。
- 原 asset、revision、check 和历史 handoff 不可变；所有引用必须在同一 operator/tenant/project。
- 修改复用项采用 copy-on-write：产生本轮新 asset/draft，独立检查；不能通过后继编辑修改前轮正文。跨 asset 来源使用独立 `derived_from_revision_id`，不混入同 asset 的 revision 链。

## 可复用输入描述

新增版本化 typed descriptor 与 digest；保存两者并核验同 digest 不同描述的冲突。不使用任意报告全文或可变项目设置作为输入。

描述至少包含：

1. operator、tenant、project 与冻结 document key、内容类型、产品、市场、语言、planner version。
2. 排序去重的冻结 source-version 依赖及其精确证据引用/quote；证据选择顺序有意义时保持顺序。release 作为来源保留，依赖完全相同且在双方 release 中时可不因 release ID 单独变化而失效。
3. 来源于 `get_cycle_settings` 的内容相关冻结配置、语义 brief、适用问题簇/文档维度。账号池、预算、报告时区等运营字段不驱动重新生成。
4. 明确的生成/证据选择/检查/修复/输出 schema 策略版本及 deliberate generation-policy revision。

原始生成 payload 中的操作 ID、brief 时间和可变来源名称必须与 descriptor 语义一致：非语义字段从模型 payload 移除，真正影响内容的字段冻结并纳入描述。不能先忽略字段做缓存，实际生成却继续使用它。

当前测量没有 evaluation/optimization 角色，冻结评估数据隔离无法据此证明。因此测量问题、逐题答案、比较键及其派生结论均不得进入本切片的复用或优化输入；后续单独实现明确角色和非冻结输入模型。

## 活体校验和原子性

当前来源用途不是缓存键，而是每次复用、封存、分发和发送前的活体校验：项目有效、来源 active/public、当前版本仍匹配、证据/quote 有效且属于目标冻结 release。用途改变只阻断受影响分支，不改写历史。

建立按作用域和 fingerprint 索引的 registry，禁止每个分支扫描全部历史 execution JSON。成功检查与 registry 的候选登记在同一事务；候选手工修订时防止新周期继续选取已被替代的版本。

`prepare_or_reuse` 原子核验目的 execution 的状态、租约、来源及原候选 check，保存本轮复用绑定。并发首次生成应以实际 execution/item 的 fenced reservation 收敛；持有者崩溃可接管，旧执行者不能将迟到结果登记为当前候选。内存实现使用同等原子边界。跨仓储锁顺序须先固定并测试，不能靠 API 分步先查后写保证并发安全。

Rust 决定复用并返回真实 ready 状态，MemeLoop 跳过已 ready 分支并按原路径封存/展开；不伪造模型完成。无模型配置时仍应允许仅复用的准备/恢复，真正需要生成的分支保留明确缺失能力。

## 历史兼容

新 JSON 字段默认缺省。历史分支仅在能由其持久冻结设置、manifest、brief/quote 和成功 check 重建完整描述时，增加独立 sidecar 索引；不改写历史 execution。

已知相同历史分支但证据不足时，将该分支标记复用依据不足，不能静默生成新身份并再发布。真正的新分支或有明确语义变化的分支按正常生成路径处理。已经封存的分发 handoff 保持不变。

## 必须实际执行的验收

- 连续两个真实自动周期：输入未变时 revision/variant/intent 相同，本轮覆盖独立，模型调用和发布 command 不增加。
- 原 intent 为 unknown 时继续查回，不新增发送。
- 内容依赖/策略变化只影响对应分支；预算、账号、报告设置变化不重生成正文。
- 来源在复用提交、封存、发送前撤销均阻止公开操作。
- 多副本首次生成/复用竞争、租约过期、重启、迟到结果只有一个有效绑定。
- 跨项目/租户引用拒绝；复用项编辑不改变旧 asset、revision、handoff。
- 原两轮自动修复及 stale-check fencing 保持有效。
- 历史可证明分支复用；不可证明分支不自动再次发布。
- PostgreSQL 重启回归和原生 MemeLoop skip-ready/closure 必须显式运行，不能只算默认 ignored 测试。

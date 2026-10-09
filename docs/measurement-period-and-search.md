# 独立测量报告与传统搜索实施说明

产品基线不变。本文件记录新增接口与接手边界；当前验证和未完成范围分别查 WORKLOG、TODO。

## 独立时间窗口报告

独立话题测量不要求企业资料或优化周期。`MeasurementPeriodReport` 独立保存窗口、时区、冻结样本及证据截止，不创建伪周期，也不改写原测量状态。后续解析作为有来源的补充结果；失败解析不覆盖既有有效结果。

接口以 `/api/v1` 为前缀，作用域来自当前登录主体与项目：

- `GET /projects/{project_id}/measurement-report-preview`：默认最近七天，或明确窗口预览。
- `GET /projects/{project_id}/measurement-reports`：已保存快照。
- `POST /projects/{project_id}/measurement-reports`：保存明确窗口，重放复用不可变结果；更正创建新版本。
- `GET /measurement-reports/{report_id}`：读取已保存报告。

`/reports` 首屏提供独立测量预览与保存历史。周期报告仍保留原路径；独立窗口报告没有伪造周期 CSV/PDF 导出。原观察时间、原文保存时间、解析完成时间分别参与截止校验。

P00 沿用 `report_get`、`report_preview`、`report_reduce`，用 `kind: measurement_period` 选择新能力；默认仍为原周期行为。预览可默认最近七天，保存必须传预览返回的确切窗口。Host Ops 版本为 `geo.hostops.v18`，仍为 45 项；API 和 bundle 必须成套部署。模型返回值省略非优化用途的问题、答案、引用和出处细节，保留报告身份与权威计数；授权用户界面仍可查看完整评估报告。

主要入口为 `crates/domain/src/measurement_report.rs`、`crates/api/src/measurement_reports.rs`、`crates/persistence/src/measurement_report.rs`，迁移 `0048`。

## 传统搜索观察

`SerpMeasurement` 与 AI 渠道账号、AI 引用和站长数据分开。当前首个适配采用 DataForSEO Google Organic Standard 的异步任务 API；普通 LLM 回答不替代搜索结果。

接受任意查询词、可选 URL/域名及受信问题引用。持久保存精确查询、来源键、协议、计划时间与幂等请求；来源键和协议共同匹配运行配置，相同协议的不同来源不会在重启后混用。地区、语言、设备等请求条件与实际返回条件分别保存；实际条件缺失保持未知。

后台流程为受理排队、持久发送意图、一次提交、原始响应入库、只读轮询、解析观察。发送结果未知不自动再次付费提交；原始部分响应和失败证据先保存，不能先解析后丢弃。待处理任务持久保存下次读取时间，进程重启不丢失轮询进度；取消后的迟到证据可归档，但不复活任务。

排名区分自然排名、绝对位置与广告等其他结果。只有完整、连续的请求深度证据才能支持范围内未出现的结论；部分或截断响应不能冒充全量未命中。重新解释已存结构化搜索响应不新增供应商任务。这是供应商结构化 JSON 的协议解码，不替代 AI 网页回答的模型语义解析。

测量页面 `?tab=search` 提供来源能力、关键词输入、历史、排名与原始证据。未配置来源时不允许发起任务，但保留历史查询。当前应用只装配持久服务和空来源表，尚无面向用户的搜索凭据配置、真实供应商验收、P00 搜索工具或报告搜索列项。

主要入口：

- `crates/domain/src/serp.rs`：协议、任务、证据与仓储契约。
- `crates/provider/src/dataforseo.rs`：HTTP 薄适配与原始响应。
- `crates/api/src/serp.rs`、`serp_dataforseo.rs`：作用域服务、恢复和结果映射。
- `crates/persistence/src/serp.rs`、迁移 `0047`：内存与 PostgreSQL。
- `apps/web/src/pages/SerpMeasurementPanel.tsx`：搜索测量详情界面。

生产接入必须显式提供来源配置与合法凭据，不把测试注入传输、合成排名或已通过契约测试写成真实搜索已完成。

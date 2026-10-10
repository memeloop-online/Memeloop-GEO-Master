# 网页搜索流观测契约

本文件描述当前实现，不变更产品规格。浏览器夹具与协议测试通过不等于真实账号测量验收。

## 执行路径

消费端网页保持独立观测面，不回退到模型 API。浏览器恢复既有加密账号会话，由网页自身发起请求；连接器不复制 Cookie 到独立 HTTP 客户端，也不自行构造登录协议。

1. 打开新会话，按页面实际选项精确选择冻结模型，不自动替换模型。
2. 启用页面搜索模式，填写冻结问题并点击一次发送。
3. 在发送前监听固定端点，只接受模型、原始问题、新会话及搜索选项相符的浏览器请求。
4. 解码 Connect JSON 响应，关联聊天、回答消息和搜索块；合并增量文本，要求最终回答完成。开关开启、HTTP 200 或流结束均不能单独证明执行了搜索。
5. 从回答实际使用的搜索引用提取 URL；缺失或不一致的证据不能升级为有效测量。
6. Rust 校验冻结协议、账号、执行来源、连接器版本、问题摘要及时间，再保存供报告汇聚的观察。未取得有效证据记为缺测，不计作未提及。

## 协议边界

- 请求和响应使用五字节帧头：一字节标志和四字节大端长度，正文为 UTF-8 JSON。
- 仅支持未压缩 JSON 消息及合法结束帧；压缩、未知标志、截断、尾随数据、服务端错误和超界数据拒绝解析。
- 原始 protobuf JSON 可能省略默认值；字段掩码是逗号分隔字符串，不是客户端解码后的 `paths` 对象。
- 文本增量按提供方操作语义合并，不能把中间答案当完整答案；引用必须属于同一回答。
- 传输设置截止时间并在退出时移除监听。实测发现 Chromium 对 `+json` 的普通 `response.body()` 读取会把帧头高位字节转换为 UTF-8 替换字符，因此实际页面改用 CDP `Network.streamResourceContent` 的 base64 原始字节；不修改网页请求、Cookie 或响应。不支持该能力时记为缺测，不回退到已损坏的文本路径。观测器字节上限不是整个 Chromium 进程的内存上限，后续容量验收需覆盖此边界。

## 证据版本

新增 AI 提取路径：浏览器保留请求/流完整性关联，模型解释响应结构，通用 JSON Pointer 校验器仅核对来源存在、消息归属值及原文片段，不声称确定性证明模型的语义判断。优先使用同一登录上下文中的独立解析会话，失败时使用部署配置的低成本模型；两种解析方式均不是新的测量样本，测量平台/观测面不改变。旧字段归并器只保留历史合成回归，不作为生产回退。

AI 路径使用 `geo.measure.official_search.v3`，事件来源为 `provider_connect_stream_ai`，保存实际聊天/消息/块标识、原测量请求摘要与时间，并绑定解析模型、提示版本及脱敏源摘要。不虚构供应商 event offset。另存唯一 `observation_extraction` 审计，`method=llm_grounded`，记录原文定位、有界证据及可重放的脱敏 `source_json`；服务端同时校验冻结测量字段、真实 runner 来源、源 JSON 结构和摘要绑定。源 JSON 仅保存在租户作用域内的证据记录，不进入公开仓库或日志。

解析模型配置仅从服务端 `GEO_OBSERVATION_AI_BASE_URL`、`GEO_OBSERVATION_AI_API_KEY`、`GEO_OBSERVATION_AI_MODEL` 读取。没有可用解析结果时仍为未验证，不转为“未提及”。解析输入不包含凭据、账号身份字段或私有推理内容；模型输出不能引入源中不存在的回答和 URL。合成测试不能替代实际账号及模型验收。

保留已有 `geo.measure.official_search.v1`。对于只提供聊天/消息/块标识的 Connect 通道，使用 `geo.measure.official_search.v2`，不虚构 `request_id` 或 `event_id`。

v2 的 `search_event` 保存：

- `kind=official_search_event`、`source=provider_connect_stream`、`provenance=live`；
- 实际 `chat_id`、`message_id`、`block_id`、十进制字符串 `event_offset`；
- 浏览器本地 `observed_at`，不宣称是供应商发生时间；
- 实际匹配请求的 `request_model` 与原始问题 UTF-8 SHA-256。

外层仍绑定目标、账号、供应方、模型、网页观测面、搜索模式、协议、问题集、市场、语言、排期、样本序号及连接器版本。服务端拒绝未知字段、版本与事件类型混用、摘要不符及越界时间。宿主另行校验真实 runner 来源；适配器自报 `live` 不能替代宿主验证。

## 验证与接手

- 传输：`packages/browser-runner/test/connect-json.test.mjs`、`connect-browser-capture.test.mjs`。
- 页面交互与答案合并：浏览器 runner 中对应消费端 Connect 搜索测试。
- Rust 接收边界：`crates/api/tests/measurement_execution.rs`。
- 真实账号验收仍需验证登录恢复、所选模型、实际搜索、完整答案、引用和报告归属。默认能力仍标注未实测，不能仅凭测试服务器的点击/响应夹具宣称真实服务可用。
- 测试和公开文档仅保存合成内容；账号、Cookie、实际问题、原始抓取材料及私有部署配置不得提交。

import { useState } from "react";
import {
  Button,
  Card,
  Field,
  Input,
  MessageBar,
  MessageBarBody,
  ProgressBar,
} from "@fluentui/react-components";
import {
  AddRegular,
  ArrowRightRegular,
  CheckmarkRegular,
} from "@fluentui/react-icons";

const steps = [
  {
    title: "品牌与来源",
    helper:
      "品牌名与至少一个知识来源为必填项。上传后会在后台解析，你可以继续配置。",
  },
  {
    title: "市场与目标",
    helper: "选择产品、市场、语言与目标用户；最多添加 5 个竞品。",
  },
  {
    title: "资源与预算",
    helper: "可选择客户自有、总部资源或混合模式。默认保留 20% 预算用于测量。",
  },
  {
    title: "检查并启动",
    helper: "确认预计导入、问题、测量、生成和发布任务量，然后持续自动运行。",
  },
];

export function SetupPage() {
  const [current, setCurrent] = useState(0);
  const [brand, setBrand] = useState("Northstar AI 助手");
  const [source, setSource] = useState("https://northstar.example.com");
  const isLast = current === steps.length - 1;
  return (
    <div className="setup-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">P01 · 首次配置</p>
          <h1>创建并启动项目</h1>
          <p>完成四步配置后，系统会自动运行知识、测量、内容和发布闭环。</p>
        </div>
      </section>
      <Card className="setup-card">
        <div className="setup-steps">
          {steps.map((step, index) => (
            <button
              key={step.title}
              onClick={() => setCurrent(index)}
              className={
                index === current ? "current" : index < current ? "done" : ""
              }
            >
              <span>{index < current ? <CheckmarkRegular /> : index + 1}</span>
              {step.title}
            </button>
          ))}
        </div>
        <ProgressBar
          value={(current + 1) / steps.length}
          aria-label={`第 ${current + 1} 步，共 ${steps.length} 步`}
        />
        <div className="setup-form">
          <p className="eyebrow">
            第 {current + 1} 步 / {steps.length} 步
          </p>
          <h2>{steps[current].title}</h2>
          <p>{steps[current].helper}</p>
          {current === 0 && (
            <div className="field-stack">
              <Field label="品牌名称" required>
                <Input
                  value={brand}
                  onChange={(_, data) => setBrand(data.value)}
                />
              </Field>
              <Field
                label="知识来源"
                required
                hint="网页、文件、FAQ 或产品资料；单文件上限 100MB。"
              >
                <Input
                  value={source}
                  onChange={(_, data) => setSource(data.value)}
                  contentAfter={
                    <Button
                      appearance="subtle"
                      icon={<AddRegular />}
                      aria-label="添加来源"
                    />
                  }
                />
              </Field>
              <MessageBar intent="info">
                <MessageBarBody>
                  文件解析将在后台运行。你无需等待，也不需要逐篇确认。
                </MessageBarBody>
              </MessageBar>
            </div>
          )}
          {current === 1 && (
            <div className="field-grid">
              <Field label="目标产品">
                <Input defaultValue="Northstar AI 助手" />
              </Field>
              <Field label="市场">
                <Input defaultValue="中国大陆" />
              </Field>
              <Field label="语言">
                <Input defaultValue="简体中文" />
              </Field>
              <Field label="目标用户">
                <Input defaultValue="企业知识管理团队" />
              </Field>
            </div>
          )}
          {current === 2 && (
            <div className="field-grid">
              <Field label="资源模式">
                <Input defaultValue="混合：客户自有 + 总部资源" />
              </Field>
              <Field label="月度预算">
                <Input defaultValue="¥ 60,000" />
              </Field>
              <Field label="测量预留">
                <Input defaultValue="20%（¥ 12,000）" />
              </Field>
            </div>
          )}
          {current === 3 && (
            <div className="review-grid">
              <div>
                <span>预计导入</span>
                <strong>12 个来源</strong>
              </div>
              <div>
                <span>候选问题</span>
                <strong>100 个</strong>
              </div>
              <div>
                <span>基线样本</span>
                <strong>500 次</strong>
              </div>
              <div>
                <span>首批内容</span>
                <strong>8 个版本</strong>
              </div>
              <div>
                <span>发布任务</span>
                <strong>14 项</strong>
              </div>
              <MessageBar intent="warning">
                <MessageBarBody>
                  启动后，自动检查通过的内容会按排期发布；未通过的内容会自动阻断。
                </MessageBarBody>
              </MessageBar>
            </div>
          )}
        </div>
        <footer className="setup-footer">
          <Button
            appearance="secondary"
            disabled={current === 0}
            onClick={() => setCurrent((value) => value - 1)}
          >
            上一步
          </Button>
          <Button
            appearance="primary"
            onClick={() => !isLast && setCurrent((value) => value + 1)}
            icon={isLast ? <CheckmarkRegular /> : <ArrowRightRegular />}
          >
            {isLast ? "保存并启动" : "下一步"}
          </Button>
        </footer>
      </Card>
    </div>
  );
}

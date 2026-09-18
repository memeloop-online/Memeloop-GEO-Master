import { Button, Tooltip } from "@fluentui/react-components";
import { ArrowRightRegular } from "@fluentui/react-icons";
import { useParams } from "react-router-dom";
import type { LoopStep } from "../data/demo";
import { StatusPill } from "./StatusPill";

const targetByStep: Record<string, string> = {
  knowledge: "knowledge",
  questions: "ask",
  baseline: "measurement",
  strategy: "campaigns",
  content: "content",
  checks: "content",
  schedule: "publications",
  publish: "publications",
  verify: "publications",
  remeasure: "measurement",
  optimize: "campaigns",
};

export function LoopProgress({ steps }: { steps: LoopStep[] }) {
  const { tenantId, projectId } = useParams();
  const base = `/app/${tenantId}/${projectId}`;
  return (
    <section className="loop-section" aria-label="自动优化闭环进度">
      <div className="section-heading">
        <div>
          <h2>自动优化闭环</h2>
          <p>点击步骤查看当前阶段的筛选详情。</p>
        </div>
        <StatusPill status="active" text="持续运行" />
      </div>
      <div className="loop-scroll">
        <ol className="loop-progress">
          {steps.map((step, index) => (
            <li key={step.id} className={`loop-step loop-${step.state}`}>
              <Tooltip content={step.detail} relationship="description">
                <Button
                  as="a"
                  href={`${base}/${targetByStep[step.id]}`}
                  appearance="transparent"
                  className="loop-button"
                >
                  <span className="loop-index">{index + 1}</span>
                  <span className="loop-name">{step.label}</span>
                </Button>
              </Tooltip>
              <span className="loop-detail">{step.detail}</span>
              {index < steps.length - 1 && (
                <ArrowRightRegular className="loop-arrow" aria-hidden="true" />
              )}
            </li>
          ))}
        </ol>
      </div>
    </section>
  );
}

import {
  Button,
  Card,
  CardHeader,
  MessageBar,
  MessageBarBody,
} from "@fluentui/react-components";
import { ArrowRightRegular, InfoRegular } from "@fluentui/react-icons";
import { useParams } from "react-router-dom";
import { workbenchContent, type WorkbenchPageKey } from "../data/demo";

export function WorkbenchPage({ page }: { page: WorkbenchPageKey }) {
  const content = workbenchContent[page];
  const { id } = useParams();
  return (
    <div className="workbench-page">
      <section className="page-hero">
        <div>
          <p className="eyebrow">{content.eyebrow}</p>
          <h1>{content.title}</h1>
          <p>{content.description}</p>
        </div>
        <Button appearance="primary" icon={<ArrowRightRegular />}>
          {content.primary}
        </Button>
      </section>
      {(page === "publications" || page === "measurement") && (
        <MessageBar intent="info">
          <MessageBarBody>
            <InfoRegular /> 演示数据用于展示状态文案和交互边界；接入 API
            后会由项目范围的服务端数据替换。
          </MessageBarBody>
        </MessageBar>
      )}
      <section className="workbench-grid">
        <Card className="panel-card primary-workbench">
          <CardHeader
            header={
              <div>
                <h2>{id ? `对象 ${id}` : "工作区"}</h2>
                <p>项目数据区域</p>
              </div>
            }
          />
          <div className="workbench-placeholder">
            <div className="placeholder-lines">
              <i />
              <i />
              <i />
              <i />
            </div>
            <div className="placeholder-body">
              <h3>此页面已接入工作台路由</h3>
              <p>
                后续工作包将把 API、加载、空状态、权限和后台事件接入此区域。
              </p>
            </div>
          </div>
        </Card>
        <Card className="panel-card">
          <CardHeader
            header={
              <div>
                <h2>当前状态</h2>
                <p>演示项目</p>
              </div>
            }
          />
          <ul className="detail-list">
            {content.items.map((item) => (
              <li key={item}>
                <span aria-hidden="true" />
                {item}
              </li>
            ))}
          </ul>
        </Card>
      </section>
    </div>
  );
}

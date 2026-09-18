import { Button, Card } from "@fluentui/react-components";
import { HomeRegular } from "@fluentui/react-icons";

export function NotFoundPage() {
  return (
    <main className="not-found">
      <Card>
        <p className="eyebrow">404</p>
        <h1>找不到这个页面</h1>
        <p>该地址可能已移动，或你没有访问该项目的权限。</p>
        <Button
          as="a"
          href="/app/acme/northstar/overview"
          appearance="primary"
          icon={<HomeRegular />}
        >
          返回项目总览
        </Button>
      </Card>
    </main>
  );
}

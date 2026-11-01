import {
  MessageContent,
  type MessageContentProps,
} from "@memeloop/react-ui/chat";
import Markdown from "react-markdown";
import remarkGfm from "remark-gfm";
import "./AgentMessageContent.css";

/** Format assistant prose through the native chat's content slot. */
export function AgentMessageContent({
  message,
  ...props
}: MessageContentProps) {
  const truncation = message.metadata?.displayTruncation as
    { contentTruncated?: boolean } | undefined;
  if (
    message.role !== "assistant" ||
    message.metadata?.originalRole === "error" ||
    message.metadata?.originalRole === "tool" ||
    message.presentations?.length ||
    truncation?.contentTruncated === true ||
    !message.content.trim()
  ) {
    return <MessageContent message={message} {...props} />;
  }

  return (
    <div className="agent-message-markdown">
      <Markdown
        remarkPlugins={[remarkGfm]}
        skipHtml
        components={{
          a: ({ node: _node, ...props }) => {
            if (!props.href) return <span>{props.children}</span>;
            const external = /^(https?:)?\/\//i.test(props.href);
            return (
              <a
                {...props}
                target={external ? "_blank" : undefined}
                rel={external ? "noopener noreferrer" : undefined}
              />
            );
          },
          // Model-authored images must not contact third parties on display.
          img: ({ alt }) => <span>{alt}</span>,
          table: ({ children }) => (
            <div className="agent-message-table" tabIndex={0}>
              <table>{children}</table>
            </div>
          ),
        }}
      >
        {message.content}
      </Markdown>
    </div>
  );
}

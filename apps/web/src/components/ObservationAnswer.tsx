import Markdown from "react-markdown";
import { safeOriginalPublicUrl } from "../pages/PublicationLookupPanel";
import "./ObservationAnswer.css";

/** Format saved answer text without interpreting its evidence or citation IDs. */
export function ObservationAnswer({ text }: { text: string }) {
  return (
    <div className="observation-answer">
      <Markdown
        components={{
          a: ({ href, children }) => {
            const url = safeOriginalPublicUrl(href ?? null);
            return url ? (
              <a href={url} target="_blank" rel="noopener noreferrer">
                {children}
              </a>
            ) : (
              <span>{children}</span>
            );
          },
          // Answer images remain descriptive text; rendering an answer must not
          // make requests to arbitrary third-party image hosts.
          img: ({ alt }) => <span>{alt}</span>,
        }}
      >
        {text}
      </Markdown>
    </div>
  );
}

import { afterEach, describe, expect, it } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { ObservationAnswer } from "./ObservationAnswer";

afterEach(cleanup);

describe("saved observation answer formatting", () => {
  it("renders headings, paragraphs, lists, emphasis and code from the saved text", () => {
    const { container } = render(
      <ObservationAnswer
        text={
          "## Summary\n\nFirst **important** paragraph.\nSecond line.\n\nAnother paragraph.\n\n- First item\n- Second item\n\n1. First step\n2. Second step\n\nUse `sample()` here.\n\n```ts\nconst sample = 1;\n```"
        }
      />,
    );
    expect(
      screen.getByRole("heading", { name: "Summary", level: 2 }),
    ).toBeVisible();
    expect(screen.getByText("important").tagName).toBe("STRONG");
    expect(screen.getByText("Another paragraph.").tagName).toBe("P");
    expect(screen.getAllByRole("list")).toHaveLength(2);
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
    expect(screen.getByText("sample()").tagName).toBe("CODE");
    expect(container.querySelector("pre > code")?.textContent).toBe(
      "const sample = 1;\n",
    );
  });

  it("escapes HTML and only opens credential-free absolute HTTP links", () => {
    const { container } = render(
      <ObservationAnswer
        text={
          '<script>unsafe()</script>\n\n<img src="https://example.test/raw.png" onerror="unsafe()">\n\n[Source](https://example.test/source) [Insecure source](http://example.test/source) [Script](javascript:unsafe) [Data](data:text/html,unsafe) [Local](/settings) [Credentials](https://user:secret@example.test/source)\n\n![Image description](https://example.test/image.png)'
        }
      />,
    );
    expect(container).toHaveTextContent("<script>unsafe()</script>");
    expect(container).toHaveTextContent(
      '<img src="https://example.test/raw.png" onerror="unsafe()">',
    );
    expect(container.querySelector("script, img")).toBeNull();
    expect(screen.getAllByRole("link")).toHaveLength(2);
    const link = screen.getByRole("link", { name: "Source" });
    expect(link).toHaveAttribute("href", "https://example.test/source");
    expect(link).toHaveAttribute("target", "_blank");
    expect(link).toHaveAttribute("rel", "noopener noreferrer");
    for (const name of ["Script", "Data", "Local", "Credentials"]) {
      expect(screen.getByText(name).tagName).toBe("SPAN");
    }
    expect(screen.getByText("Image description")).toBeVisible();
  });

  it("leaves unmapped citation markers as text instead of assigning evidence links", () => {
    render(<ObservationAnswer text="Statement [1] and another [8]." />);
    expect(screen.getByText("Statement [1] and another [8].")).toBeVisible();
    expect(screen.queryByRole("link")).not.toBeInTheDocument();
  });
});

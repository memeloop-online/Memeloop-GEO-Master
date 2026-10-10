import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { CsvEvidenceTable } from "./CsvEvidenceTable";

describe("CSV original values", () => {
  it("preserves duplicate headers, empty cells, leading zeros and inert formulas", () => {
    render(
      <CsvEvidenceTable
        text={JSON.stringify({
          headers: ["价格", "价格", "", "换行"],
          values: ["001.00 元", "=1+1", "", "  first\nsecond  "],
        })}
      />,
    );
    expect(screen.getAllByRole("columnheader")).toHaveLength(4);
    expect(screen.getAllByText("价格")).toHaveLength(2);
    expect(screen.getByText("001.00 元")).toBeInTheDocument();
    expect(screen.getByText("=1+1")).toBeInTheDocument();
    expect(screen.getByLabelText("空单元格")).toBeInTheDocument();
    expect(screen.getAllByRole("cell")[3].textContent).toBe(
      "  first\nsecond  ",
    );
    expect(screen.getByRole("region")).toHaveAttribute("tabindex", "0");
  });
  it.each([
    "not json",
    '{"headers":["x"],"values":[]}',
    '{"headers":["x"],"values":[1]}',
  ])("does not invent cells for malformed data: %s", (text) => {
    render(<CsvEvidenceTable text={text} />);
    expect(screen.queryByRole("table")).toBeNull();
    expect(screen.getByText(/表格片段格式不完整/)).toBeInTheDocument();
  });
});

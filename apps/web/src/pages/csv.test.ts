import { describe, expect, it } from "vitest";
import { serializeCsv } from "./csv";

describe("shared report CSV serializer", () => {
  it("retains the existing BOM, CRLF, quote escaping and spreadsheet safety bytes", () => {
    expect(
      serializeCsv([
        ["plain", 'comma, "quote"\nline', null, 0],
        ["=1+2", " \u0000+formula", "@name", "-formula"],
        ["\tplain", "\rplain", "\nplain", "safe"],
      ]),
    ).toBe(
      '\uFEFF"plain","comma, ""quote""\nline","","0"\r\n' +
        '"\'=1+2","\' \u0000+formula","\'@name","\'-formula"\r\n' +
        '"\'\tplain","\'\rplain","\'\nplain","safe"\r\n',
    );
  });
});

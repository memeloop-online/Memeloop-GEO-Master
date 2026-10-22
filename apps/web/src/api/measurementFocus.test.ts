import { describe, expect, it } from "vitest";
import {
  isMeasurementFocus,
  measurementFocusNames,
  measurementFocusPatch,
} from "./measurementFocus";

describe("measurement focus", () => {
  it("prefers the specific product without treating its brand as an alias", () => {
    expect(
      measurementFocusNames({
        brand_name: "Example",
        product_name: "Example X",
      }),
    ).toEqual(["Example X"]);
    expect(measurementFocusNames({ brand_name: "Example" })).toEqual([
      "Example",
    ]);
    expect(measurementFocusNames(undefined)).toEqual([]);
  });

  it("matches explicit names without merging models, case or substrings", () => {
    expect(isMeasurementFocus(" Example X ", ["Example X"])).toBe(true);
    expect(isMeasurementFocus("Example X Pro", ["Example X"])).toBe(false);
    expect(isMeasurementFocus("Example", ["Example X"])).toBe(false);
    expect(isMeasurementFocus("example x", ["Example X"])).toBe(false);
  });

  it("patches only the focus fields at the edited revision", () => {
    expect(measurementFocusPatch(4, " Example ", " X ")).toEqual({
      revision: 4,
      settings: { brand_name: "Example", product_name: "X" },
    });
    expect(measurementFocusPatch(4, "", "")).toEqual({
      revision: 4,
      settings: { brand_name: "", product_name: null },
    });
  });
});

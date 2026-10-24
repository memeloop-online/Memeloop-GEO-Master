import { describe, expect, it } from "vitest";
import {
  isMeasurementFocus,
  measurementFocusTargets,
  measurementFocusPatch,
} from "./measurementFocus";
import type { MeasurementTarget } from "./projects";

const target: MeasurementTarget = {
  name: "Example X",
  kind: "product",
  aliases: ["Example short"],
};

describe("measurement targets", () => {
  it("uses independent targets without inferring enterprise metadata", () => {
    const settings = {
      brand_name: "Example",
      product_name: "Example X",
      measurement_targets: [],
    };
    expect(measurementFocusTargets(settings)).toEqual([]);
    expect(measurementFocusTargets(undefined)).toEqual([]);
    expect(measurementFocusTargets({ measurement_targets: [target] })).toEqual([
      target,
    ]);
  });

  it("matches exact trimmed names or explicit aliases only within the configured kind", () => {
    for (const name of [" Example X ", "Example short"]) {
      expect(isMeasurementFocus({ name, kind: "product" }, [target])).toBe(
        true,
      );
    }
    for (const name of ["Example", "example x", "Example X Pro"]) {
      expect(isMeasurementFocus({ name, kind: "product" }, [target])).toBe(
        false,
      );
    }
    expect(
      isMeasurementFocus({ name: target.name, kind: "brand" }, [target]),
    ).toBe(false);
    expect(
      isMeasurementFocus({ name: "Example short", kind: "restaurant" }, [
        target,
      ]),
    ).toBe(false);
  });

  it("replaces only target settings at the edited revision and clears with an empty list", () => {
    expect(
      measurementFocusPatch(4, [
        { ...target, name: " Example X ", aliases: [" short ", "", "short"] },
      ]),
    ).toEqual({
      revision: 4,
      settings: { measurement_targets: [{ ...target, aliases: ["short"] }] },
    });
    expect(measurementFocusPatch(4, [])).toEqual({
      revision: 4,
      settings: { measurement_targets: [] },
    });
    expect(target.aliases).toEqual(["Example short"]);
  });
});

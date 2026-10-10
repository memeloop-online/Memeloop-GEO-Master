import assert from "node:assert/strict";
import { test } from "node:test";
import { inspectKimiMeasurementOptions } from "../src/kimi-connect-search.mjs";

function pageFor(rows) {
  const actions = [];
  const options = rows.map((row) => ({
    isVisible: async () => row.visible !== false,
    innerText: async () => row.label,
    getAttribute: async (name) => row[name] ?? null,
  }));
  return {
    actions,
    keyboard: { press: async (key) => actions.push(key) },
    getByTestId(name) {
      if (name === "model-select-trigger") {
        return { click: async () => actions.push("open") };
      }
      assert.equal(name, "model-option");
      return {
        first: () => ({ waitFor: async () => {} }),
        all: async () => options,
      };
    },
  };
}

test("model discovery returns only observed IDs and visible labels, without submission", async () => {
  const page = pageFor([
    {
      "data-moon-key": "observed-alpha",
      label: " Alpha ",
      "aria-selected": "true",
    },
    { "data-moon-key": "observed-beta", label: "Beta" },
    { "data-moon-key": "hidden", label: "Hidden", visible: false },
  ]);
  assert.deepEqual(await inspectKimiMeasurementOptions(page), {
    models: [
      { id: "observed-alpha", label: "Alpha" },
      { id: "observed-beta", label: "Beta" },
    ],
    selected_model: "observed-alpha",
  });
  assert.deepEqual(page.actions, ["open", "Escape"]);
});

test("model discovery does not invent selection or fallback models", async () => {
  const page = pageFor([{ "data-moon-key": "observed", label: "Observed" }]);
  assert.equal(
    (await inspectKimiMeasurementOptions(page)).selected_model,
    null,
  );
  assert.equal(await inspectKimiMeasurementOptions(pageFor([])), null);
});

test("malformed, duplicate and ambiguous menu entries fail closed and close menu", async () => {
  for (const rows of [
    [{ "data-moon-key": "../bad", label: "Bad" }],
    [{ "data-moon-key": "valid", label: "" }],
    Array(2).fill({ "data-moon-key": "same", label: "Same" }),
    [
      { "data-moon-key": "one", label: "One", "aria-checked": "true" },
      { "data-moon-key": "two", label: "Two", "aria-checked": "true" },
    ],
  ]) {
    const page = pageFor(rows);
    assert.equal(await inspectKimiMeasurementOptions(page), null);
    assert.equal(page.actions.at(-1), "Escape");
  }
});

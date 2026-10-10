import assert from "node:assert/strict";
import { test } from "node:test";
import { inspectKimiMeasurementOptions } from "../src/kimi-connect-search.mjs";
import { createRunner } from "../src/runner.mjs";

const observed = {
  models: [{ id: "observed-model", label: "Observed model" }],
  selected_model: "observed-model",
};

function inspectionFixture(operations, inspector) {
  const state = {
    identity: {
      platform_account_id: "synthetic-account",
      display_name: "Fixture",
    },
    options: observed,
    inspections: 0,
    executions: 0,
  };
  const runner = createRunner({
    platformAdapters: {
      fixture: {
        connectorVersion: "fixture.inspection.v1",
        entry: "https://example.invalid/",
        operations,
        // A self-reported flag cannot replace an installed inspector.
        model_discovery_supported: true,
        inspectMeasurementOptions:
          inspector === "installed"
            ? async () => {
                state.inspections++;
                return state.options;
              }
            : inspector,
        identify: async () => state.identity,
        execute: async () => {
          state.executions++;
          throw new Error("model inspection must not execute an operation");
        },
      },
    },
    browserType: {
      launch: async () => ({
        newContext: async () => ({
          newPage: async () => ({
            url: () => "https://example.invalid/",
            goto: async () => {},
          }),
          storageState: async () => ({ cookies: [], origins: [] }),
          close: async () => {},
        }),
        close: async () => {},
      }),
    },
  });
  const create = () =>
    runner.create({
      session_id: "inspection",
      platform: "fixture",
      storage_state: { cookies: [], origins: [] },
    });
  return { runner, state, create };
}

const runnerError = (status, code) => (error) =>
  error.status === status && error.code === code;

test("installed model inspection is advertised and dispatched independently of measure", async () => {
  for (const operations of [[], ["measure"]]) {
    for (const inspector of [
      "installed",
      undefined,
      null,
      true,
      {},
      "not-a-function",
    ]) {
      const { runner, state, create } = inspectionFixture(
        operations,
        inspector,
      );
      try {
        const connector = runner.capabilities().connectors[0];
        assert.equal(
          connector.model_discovery_supported,
          inspector === "installed",
        );
        assert.deepEqual(connector.operations, operations);
        assert.equal(connector.verified, false);
        await create();
        await assert.rejects(
          runner.measurementOptions("inspection"),
          runnerError(409, "login_required"),
        );
        await runner.complete("inspection");
        if (inspector === "installed") {
          assert.deepEqual(
            await runner.measurementOptions("inspection"),
            observed,
          );
          assert.equal(state.inspections, 1);
        } else {
          await assert.rejects(
            runner.measurementOptions("inspection"),
            runnerError(422, "measurement_options_unavailable"),
          );
          assert.equal(state.inspections, 0);
        }
        if (!operations.length) {
          const result = await runner.execute({
            execution_id: "unsupported-measure",
            session_id: "inspection",
            operation: "measure",
            payload: {},
          });
          assert.equal(result.status, "unsupported");
          assert.equal(result.reason, "operation_not_supported");
        }
        assert.equal(state.executions, 0);
        await runner.close("inspection");
      } finally {
        await runner.shutdown();
      }
    }
  }
});

test("model inspection keeps identity and unavailable-result guards without sending", async () => {
  const { runner, state, create } = inspectionFixture([], "installed");
  try {
    await create();
    await runner.complete("inspection");
    const identity = state.identity;
    for (const changed of [
      null,
      { ...identity, platform_account_id: "other-account" },
    ]) {
      state.identity = changed;
      await assert.rejects(
        runner.measurementOptions("inspection"),
        runnerError(409, "account_mismatch"),
      );
    }
    assert.equal(state.inspections, 0);
    state.identity = identity;
    state.options = null;
    await assert.rejects(
      runner.measurementOptions("inspection"),
      runnerError(422, "measurement_options_unavailable"),
    );
    state.options = observed;
    assert.deepEqual(await runner.measurementOptions("inspection"), observed);
    assert.equal(state.executions, 0);
  } finally {
    await runner.shutdown();
  }
});

test("model inspection reserves its session and releases it after completion", async () => {
  const { runner, state, create } = inspectionFixture([], "installed");
  let finish;
  state.options = new Promise((resolve) => {
    finish = resolve;
  });
  try {
    await create();
    await runner.complete("inspection");
    // The inspector's returned promise is awaited within the existing busy guard.
    const result = runner.measurementOptions("inspection");
    await assert.rejects(
      runner.measurementOptions("inspection"),
      runnerError(409, "session_busy"),
    );
    await assert.rejects(
      runner.close("inspection"),
      runnerError(409, "session_busy"),
    );
    finish(observed);
    assert.deepEqual(await result, observed);
    await runner.close("inspection");
    assert.equal(state.executions, 0);
  } finally {
    finish(observed);
    await runner.shutdown();
  }
});

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

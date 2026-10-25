import assert from "node:assert/strict";
import test from "node:test";
import * as facade from "../src/adapters.mjs";
import { CONNECTOR_VERSION, adapters } from "../src/adapters.mjs";

// The registry is a public shape: `runner.capabilities()` serialises it and
// callers select adapters by key. These assertions exist so the provider split
// cannot reorder keys, drop a freeze, or silently gain a method.
const REGISTRY_ORDER = [
  "zhihu",
  "baidu_creator",
  "xiaohongshu",
  "kimi",
  "glm",
  "deepseek",
  "doubao",
  "qianwen",
  "yuanbao",
];
const CONTENT_KEYS = [
  "loginEntryAvailable",
  "loginSupported",
  "connectorVersion",
  "origin",
  "entry",
  "operations",
  "identify",
  "execute",
];
const MEASUREMENT_KEYS = [
  "origin",
  "entry",
  "loginEntryAvailable",
  "loginSupported",
  "inspectMeasurementOptions",
  "connectorVersion",
  "operations",
  "identify",
  "execute",
];
const CONSUMER_KEYS = [
  "origin",
  "entry",
  "loginEntryAvailable",
  "loginSupported",
  "connectorVersion",
  "operations",
  "identify",
  "execute",
];
const SHAPES = {
  zhihu: { keys: CONTENT_KEYS, operations: ["publish", "lookup"] },
  baidu_creator: { keys: CONTENT_KEYS, operations: ["publish", "lookup"] },
  xiaohongshu: { keys: CONTENT_KEYS, operations: ["publish", "lookup"] },
  kimi: { keys: MEASUREMENT_KEYS, operations: ["measure"] },
  glm: { keys: CONSUMER_KEYS, operations: [] },
  deepseek: { keys: MEASUREMENT_KEYS, operations: ["measure"] },
  doubao: { keys: CONSUMER_KEYS, operations: [] },
  qianwen: { keys: CONSUMER_KEYS, operations: [] },
  yuanbao: { keys: CONSUMER_KEYS, operations: [] },
};
const FACADE_EXPORTS = [
  "CONNECTOR_VERSION",
  "adapters",
  "baiduIdentity",
  "deepseekIdentity",
  "doubaoIdentity",
  "extractKimiCandidateObservation",
  "glmIdentity",
  "kimiIdentity",
  "measureDeepseek",
  "measureKimi",
  "observeKimiSearchAttempt",
  "probeDeepseekAccount",
  "probeDoubaoAccount",
  "probeGlmAccount",
  "probeKimiAccount",
  "probeOwnAccount",
  "probeQianwenAccount",
  "publishBaidu",
  "publishZhihu",
  "qianwenIdentity",
  "readbackZhihu",
  "xiaohongshuIdentity",
  "zhihuIdentity",
];

test("the registry keeps its key order", () => {
  assert.deepEqual(Object.keys(adapters), REGISTRY_ORDER);
});

test("the registry and every adapter stay frozen", () => {
  assert.ok(Object.isFrozen(adapters));
  for (const name of REGISTRY_ORDER) {
    assert.ok(Object.isFrozen(adapters[name]), name);
    assert.ok(Object.isFrozen(adapters[name].operations), `${name}.operations`);
  }
});

test("each provider keeps its shape, key order and operations", () => {
  for (const [name, shape] of Object.entries(SHAPES)) {
    const adapter = adapters[name];
    assert.deepEqual(Object.keys(adapter), shape.keys, name);
    assert.deepEqual([...adapter.operations], shape.operations, name);
  }
});

test("rich publishing is not declared by any adapter yet", () => {
  // `runner.mjs` probes `typeof adapter.executeRich`; adding the key would make
  // that capability appear without an implementation behind it.
  for (const name of REGISTRY_ORDER) {
    assert.equal("executeRich" in adapters[name], false, name);
    assert.equal(
      adapters[name].operations.includes("rich_publish"),
      false,
      name,
    );
  }
});

test("only measurement adapters expose option inspection", () => {
  for (const name of REGISTRY_ORDER) {
    const inspect = adapters[name].inspectMeasurementOptions;
    if (name === "kimi" || name === "deepseek") {
      assert.equal(typeof inspect, "function", name);
    } else {
      assert.equal(inspect, undefined, name);
    }
  }
});

test("every adapter reports the connector version", () => {
  for (const name of REGISTRY_ORDER) {
    assert.equal(adapters[name].connectorVersion, CONNECTOR_VERSION, name);
  }
});

test("the facade keeps its export set", () => {
  assert.deepEqual(Object.keys(facade).sort(), FACADE_EXPORTS);
});

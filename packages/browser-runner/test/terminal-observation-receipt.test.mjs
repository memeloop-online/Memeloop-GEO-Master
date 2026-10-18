import assert from "node:assert/strict";
import { test } from "node:test";
import { stampTerminalObservation } from "../src/runner.mjs";

const settledAt = Date.parse("2026-01-02T03:04:05.678Z");

test("live unknown receipts carry terminal observation time without promotion", () => {
  for (const reason of [
    "extraction_pending",
    "execution_deadline",
    "observation_capture_missing",
    undefined,
  ]) {
    const outcome = {
      execution_id: "terminal-attempt",
      provenance: "live",
      status: "unknown",
      evidence: [],
      ...(reason === undefined ? {} : { reason }),
    };
    assert.deepEqual(stampTerminalObservation(outcome, settledAt), {
      ...outcome,
      occurred_at: "2026-01-02T03:04:05.678Z",
    });
    assert.equal(outcome.occurred_at, undefined);
  }
});

test("terminal stamping preserves fixture provenance and exact source timestamps", () => {
  for (const outcome of [
    { provenance: "fixture", status: "unknown", evidence: [] },
    {
      provenance: "fixture",
      status: "unknown",
      evidence: [],
      occurred_at: "2026-01-01T00:00:00.123456Z",
    },
    { provenance: "live", status: "completed", evidence: [] },
    {
      provenance: "live",
      status: "completed",
      evidence: [],
      occurred_at: "2026-01-01T00:00:00.123456Z",
    },
    {
      provenance: "live",
      status: "unknown",
      evidence: [],
      occurred_at: "2026-01-01T00:00:00.123456Z",
    },
  ]) {
    assert.equal(stampTerminalObservation(outcome, settledAt), outcome);
  }
});

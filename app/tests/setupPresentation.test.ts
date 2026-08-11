import assert from "node:assert/strict";
import test from "node:test";
import { modelVerificationView } from "../src/setupPresentation.ts";
import type { ModelStatus } from "../src/contracts.ts";

function model(state: ModelStatus["state"]): ModelStatus {
  return {
    id: state,
    displayName: state,
    state,
    source: state === "ready" ? "managed" : null,
    verifiedBytes: state === "ready" ? 1 : 0,
    totalBytes: 1,
    licenseName: "Test",
    licenseUrl: "https://example.invalid",
    error: null,
  };
}

test("model verification does not invent a total while status is loading", () => {
  assert.deepEqual(modelVerificationView([], true), {
    label: "Verifying",
    ready: false,
    semanticState: "starting",
  });
});

test("model verification reports only known model counts", () => {
  assert.deepEqual(modelVerificationView([model("ready"), model("missing")], false), {
    label: "1/2",
    ready: false,
    semanticState: "stopped",
  });
  assert.deepEqual(modelVerificationView([], false), {
    label: "Unavailable",
    ready: false,
    semanticState: "stopped",
  });
});

test("model verification reports a complete verified set", () => {
  assert.deepEqual(modelVerificationView([model("ready")], false), {
    label: "Verified",
    ready: true,
    semanticState: "ready",
  });
});

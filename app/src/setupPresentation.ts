import type { ModelStatus } from "./contracts";

export type ModelVerificationView = {
  label: string;
  ready: boolean;
  semanticState: "ready" | "starting" | "stopped";
};

export function modelVerificationView(
  models: ModelStatus[],
  loading: boolean,
): ModelVerificationView {
  if (loading) {
    return { label: "Verifying", ready: false, semanticState: "starting" };
  }
  if (models.length === 0) {
    return { label: "Unavailable", ready: false, semanticState: "stopped" };
  }
  const verified = models.filter((model) => model.state === "ready").length;
  if (verified === models.length) {
    return { label: "Verified", ready: true, semanticState: "ready" };
  }
  return {
    label: `${verified}/${models.length}`,
    ready: false,
    semanticState: "stopped",
  };
}

export type ConversationState =
  | "idle"
  | "listening"
  | "thinking"
  | "speaking"
  | "interrupted"
  | "faulted";

export type TranscriptMessage = {
  role: "user" | "assistant";
  text: string;
};

export type EngineSnapshot = {
  state: ConversationState;
  turnId: number;
  revision: number;
  cancellationEpoch: number;
  partialTranscript: string;
  committedTranscript: string;
  assistantTranscript: string;
  transcript: TranscriptMessage[];
  pendingClause: string | null;
  lastError: string | null;
};

export type AudioDeviceInfo = {
  id: string;
  name: string;
  isDefault: boolean;
  isCompatible: boolean;
};

export type AudioDevices = {
  inputs: AudioDeviceInfo[];
  outputs: AudioDeviceInfo[];
};

export type AudioStatus = {
  active: boolean;
  muted: boolean;
  inputDeviceId: string;
  inputDevice: string;
  outputDeviceId: string;
  outputDevice: string;
  sampleRateHz: number;
  speechActive: boolean;
  queuedPlaybackSamples: number;
  droppedCaptureSamples: number;
  droppedPlaybackSamples: number;
  droppedAsrSamples: number;
  lastCancelToCallbackMs: number | null;
  lastError: string | null;
};

export type WorkerState =
  | "stopped"
  | "starting"
  | "ready"
  | "unhealthy"
  | "restartPending"
  | "failed";

export type DiagnosticLine = {
  stream: "stdout" | "stderr" | "supervisor";
  message: string;
};

export type WorkerStatus = {
  id: string;
  state: WorkerState;
  processId: number | null;
  restartAttempts: number;
  lastExitCode: number | null;
  diagnostics: DiagnosticLine[];
};

export type NativeRuntimeStatus = {
  llm: WorkerStatus | null;
  speech: WorkerStatus | null;
  kokoro: WorkerStatus | null;
  ttsBackend: "magpie" | "kokoro";
  vramAdmission: {
    currentUsedMib: number | null;
    projectedWarmedMib: number | null;
    limitMib: number;
    backend: "magpie" | "kokoro";
    reason: string;
  };
};

export type ModelState = "missing" | "partial" | "ready" | "corrupt";

export type ModelInstallSource = "managed" | "legacy";

export type ModelStatus = {
  id: string;
  displayName: string;
  state: ModelState;
  source: ModelInstallSource | null;
  verifiedBytes: number;
  totalBytes: number;
  licenseName: string;
  licenseUrl: string;
  error: string | null;
};

export type ModelProgress = {
  modelId: string;
  artifact: string;
  downloadedBytes: number;
  totalBytes: number;
};

export type AudioStartRequest = {
  inputDeviceId: string | null;
  outputDeviceId: string | null;
};

export type CommandError = {
  code: string;
  message: string;
};

export const initialSnapshot: EngineSnapshot = {
  state: "idle",
  turnId: 0,
  revision: 0,
  cancellationEpoch: 0,
  partialTranscript: "",
  committedTranscript: "",
  assistantTranscript: "",
  transcript: [],
  pendingClause: null,
  lastError: null,
};

export const initialRuntimeStatus: NativeRuntimeStatus = {
  llm: null,
  speech: null,
  kokoro: null,
  ttsBackend: "kokoro",
  vramAdmission: {
    currentUsedMib: null,
    projectedWarmedMib: null,
    limitMib: 23_040,
    backend: "kokoro",
    reason: "GPU memory has not been measured yet",
  },
};

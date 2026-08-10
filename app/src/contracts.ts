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
};

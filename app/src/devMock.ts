import type { InvokeArgs, InvokeOptions } from "@tauri-apps/api/core";
import { emit } from "@tauri-apps/api/event";
import { mockIPC } from "@tauri-apps/api/mocks";
import {
  initialSnapshot,
  type AudioStatus,
  type EngineSnapshot,
  type ModelStatus,
  type NativeRuntimeStatus,
  type WorkerStatus,
} from "./contracts";

function worker(id: string, state: WorkerStatus["state"]): WorkerStatus {
  return {
    id,
    state,
    processId: state === "stopped" ? null : 10_000 + id.length,
    restartAttempts: 0,
    lastExitCode: null,
    diagnostics:
      state === "stopped"
        ? []
        : [
            { stream: "supervisor", message: "worker process started" },
            ...(state === "ready" ? [{ stream: "stdout" as const, message: "ready on loopback" }] : []),
          ],
  };
}

function runtimeState(state: WorkerStatus["state"]): NativeRuntimeStatus {
  return {
    llm: worker("llm", state),
    speech: worker("speech", state),
    kokoro: worker("kokoro", state),
    profileId: "rtx3090-qwen35-q5-magpie",
    ttsBackend: "magpie",
    vramAdmission: {
      startupAdmitted: state !== "stopped",
      currentUsedMib: 3_143,
      totalMib: 24_576,
      projectedWarmedMib: 12_000,
      limitMib: 23_040,
      profileId: "rtx3090-qwen35-q5-magpie",
      reserveMib: 1_536,
      backend: "magpie",
      reason: "Qwen3.5 compatibility profile leaves room for warmed Magpie TTS",
    },
  };
}

const devices = {
  inputs: [
    { id: "wasapi:microphone", name: "Microphone", isDefault: true, isCompatible: true },
    { id: "wasapi:webcam", name: "Webcam microphone", isDefault: false, isCompatible: true },
  ],
  outputs: [
    { id: "wasapi:speakers", name: "Realtek speakers", isDefault: true, isCompatible: true },
    { id: "wasapi:headset", name: "USB headset", isDefault: false, isCompatible: true },
  ],
};

const models: ModelStatus[] = [
  ["qwen", "Qwen3.5 9B Q5_K_M compatibility profile", "Apache License 2.0"],
  ["nemotron-asr", "Nemotron 3.5 ASR", "OpenMDW 1.1"],
  ["magpie-tts", "Magpie TTS", "NVIDIA Open Model License"],
  ["nanocodec", "NVIDIA NanoCodec", "NVIDIA Open Model License"],
  ["kokoro", "Kokoro 82M INT8", "Apache License 2.0"],
].map(([id, displayName, licenseName]) => ({
  id,
  displayName,
  state: "ready",
  source: "managed",
  verifiedBytes: 1,
  totalBytes: 1,
  licenseName,
  licenseUrl: "https://huggingface.co/",
  error: null,
}));

export function installDevMock() {
  let snapshot: EngineSnapshot = structuredClone(initialSnapshot);
  let runtime = runtimeState("stopped");
  let audio: AudioStatus | null = null;
  let timers: number[] = [];

  const cancelTurnTimers = () => {
    timers.forEach((timer) => window.clearTimeout(timer));
    timers = [];
  };
  const publish = async () => emit("conversation-snapshot", structuredClone(snapshot));
  const revise = (next: Partial<EngineSnapshot>) => {
    snapshot = { ...snapshot, ...next, revision: snapshot.revision + 1 };
    void publish();
  };
  const scheduleConversation = () => {
    cancelTurnTimers();
    timers.push(
      window.setTimeout(() => {
        revise({ partialTranscript: "Give me a concise summary of the implementation status." });
      }, 450),
      window.setTimeout(() => {
        revise({
          state: "thinking",
          partialTranscript: "",
          committedTranscript: "Give me a concise summary of the implementation status.",
        });
      }, 1_150),
      window.setTimeout(() => {
        revise({
          state: "speaking",
          assistantTranscript: "The native speech pipeline, desktop controls, and verified model setup are working.",
          pendingClause: "The native speech pipeline, desktop controls, and verified model setup are working.",
        });
      }, 1_750),
      window.setTimeout(() => {
        revise({
          state: "listening",
          transcript: [
            ...snapshot.transcript,
            { role: "user", text: snapshot.committedTranscript },
            { role: "assistant", text: snapshot.assistantTranscript },
          ],
          committedTranscript: "",
          assistantTranscript: "",
          pendingClause: null,
          turnId: snapshot.turnId + 1,
        });
      }, 3_400),
    );
  };

  mockIPC(
    async (command: string, payload?: InvokeArgs) => {
      const args = payload as Record<string, unknown> | undefined;
      switch (command) {
        case "engine_snapshot":
          return structuredClone(snapshot);
        case "audio_devices":
          return structuredClone(devices);
        case "audio_status":
          return structuredClone(audio);
        case "audio_start": {
          const request = args?.request as { inputDeviceId?: string; outputDeviceId?: string } | undefined;
          const input = devices.inputs.find((device) => device.id === request?.inputDeviceId) ?? devices.inputs[0];
          const output = devices.outputs.find((device) => device.id === request?.outputDeviceId) ?? devices.outputs[0];
          audio = {
            active: true,
            muted: false,
            inputDeviceId: input.id,
            inputDevice: input.name,
            outputDeviceId: output.id,
            outputDevice: output.name,
            sampleRateHz: 48_000,
            speechActive: false,
            queuedPlaybackSamples: 0,
            droppedCaptureSamples: 0,
            droppedPlaybackSamples: 0,
            droppedAsrSamples: 0,
            lastCancelToCallbackMs: 8.4,
            lastError: null,
          };
          return structuredClone(audio);
        }
        case "audio_set_muted":
          if (!audio) throw { code: "audioNotReady", message: "Windows audio is not running." };
          audio = { ...audio, muted: Boolean(args?.muted) };
          return structuredClone(audio);
        case "audio_stop":
          audio = null;
          return null;
        case "runtime_status":
          return structuredClone(runtime);
        case "runtime_start":
          runtime = runtimeState("starting");
          window.setTimeout(() => {
            runtime = runtimeState("ready");
          }, 800);
          return structuredClone(runtime);
        case "runtime_cancel_start":
          return null;
        case "runtime_stop":
          runtime = runtimeState("stopped");
          return structuredClone(runtime);
        case "model_status":
        case "model_install_all":
        case "model_import_pack":
          return structuredClone(models);
        case "model_export_pack":
          return null;
        case "conversation_start":
          if (!audio || runtime.llm?.state !== "ready") {
            throw { code: "workersNotReady", message: "Local services are not ready." };
          }
          revise({
            state: "listening",
            turnId: snapshot.turnId + 1,
            partialTranscript: "",
            committedTranscript: "",
            assistantTranscript: "",
            pendingClause: null,
            lastError: null,
          });
          scheduleConversation();
          return structuredClone(snapshot);
        case "conversation_interrupt":
          cancelTurnTimers();
          revise({
            state: "interrupted",
            transcript: [
              ...snapshot.transcript,
              ...(snapshot.committedTranscript
                ? [{ role: "user" as const, text: snapshot.committedTranscript }]
                : []),
              ...(snapshot.assistantTranscript
                ? [{ role: "assistant" as const, text: snapshot.assistantTranscript }]
                : []),
            ],
            committedTranscript: "",
            assistantTranscript: "",
            pendingClause: null,
            cancellationEpoch: snapshot.cancellationEpoch + 1,
          });
          timers.push(window.setTimeout(() => revise({ state: "listening" }), 300));
          return null;
        case "conversation_stop":
          cancelTurnTimers();
          snapshot = {
            ...structuredClone(initialSnapshot),
            revision: snapshot.revision + 1,
            cancellationEpoch: snapshot.cancellationEpoch + 1,
          };
          await publish();
          return structuredClone(snapshot);
        default:
          throw new Error(`Unhandled FastTalk preview command: ${command}`);
      }
    },
    { shouldMockEvents: true },
  );

  // @tauri-apps/api 2.8 emits `eventId` when removing a listener, while its
  // browser mock currently looks for `id`. Translate the field so React
  // StrictMode can mount and unmount the preview listener without retaining a
  // stale callback that warns on every later snapshot.
  type MockInvoke = <T>(
    command: string,
    args?: InvokeArgs,
    options?: InvokeOptions,
  ) => Promise<T>;
  const internals = (window as unknown as { __TAURI_INTERNALS__: { invoke: MockInvoke } })
    .__TAURI_INTERNALS__;
  const invokeMock = internals.invoke;
  internals.invoke = <T>(command: string, args?: InvokeArgs, options?: InvokeOptions) => {
    if (command === "plugin:event|unlisten" && args && "eventId" in args) {
      return invokeMock<T>(command, { ...args, id: args.eventId }, options);
    }
    return invokeMock<T>(command, args, options);
  };
}

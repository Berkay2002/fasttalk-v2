import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import type {
  AudioDevices,
  AudioStartRequest,
  AudioStatus,
  EngineSnapshot,
  NativeRuntimeStatus,
} from "./contracts";

export const fastTalkApi = {
  engineSnapshot: () => invoke<EngineSnapshot>("engine_snapshot"),
  onEngineSnapshot: (handler: (snapshot: EngineSnapshot) => void): Promise<UnlistenFn> =>
    listen<EngineSnapshot>("conversation-snapshot", ({ payload }) => handler(payload)),
  audioDevices: () => invoke<AudioDevices>("audio_devices"),
  audioStart: (request: AudioStartRequest) =>
    invoke<AudioStatus>("audio_start", { request }),
  audioStatus: () => invoke<AudioStatus | null>("audio_status"),
  audioSetMuted: (muted: boolean) =>
    invoke<AudioStatus>("audio_set_muted", { muted }),
  audioStop: () => invoke<void>("audio_stop"),
  runtimeStart: () => invoke<NativeRuntimeStatus>("runtime_start"),
  runtimeStatus: () => invoke<NativeRuntimeStatus>("runtime_status"),
  runtimeStop: () => invoke<NativeRuntimeStatus>("runtime_stop"),
  conversationStart: () => invoke<EngineSnapshot>("conversation_start"),
  conversationInterrupt: () => invoke<void>("conversation_interrupt"),
  conversationStop: () => invoke<EngineSnapshot>("conversation_stop"),
};

export function errorMessage(cause: unknown): string {
  if (typeof cause === "object" && cause !== null && "message" in cause) {
    const message = (cause as { message?: unknown }).message;
    if (typeof message === "string") {
      return message;
    }
  }
  return String(cause);
}

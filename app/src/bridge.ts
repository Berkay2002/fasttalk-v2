import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { open, save } from "@tauri-apps/plugin-dialog";
import type {
  AudioDevices,
  AudioStartRequest,
  AudioStatus,
  EngineSnapshot,
  ModelProgress,
  ModelStatus,
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
  modelStatus: () => invoke<ModelStatus[]>("model_status"),
  modelInstallAll: () => invoke<ModelStatus[]>("model_install_all"),
  onModelProgress: (handler: (progress: ModelProgress) => void): Promise<UnlistenFn> =>
    listen<ModelProgress>("model-progress", ({ payload }) => handler(payload)),
  modelImportPack: async () => {
    const path = await open({
      multiple: false,
      directory: false,
      filters: [{ name: "FastTalk model pack", extensions: ["tar"] }],
    });
    return typeof path === "string"
      ? invoke<ModelStatus[]>("model_import_pack", { path })
      : null;
  },
  modelExportPack: async () => {
    const path = await save({
      defaultPath: "fasttalk-model-pack.tar",
      filters: [{ name: "FastTalk model pack", extensions: ["tar"] }],
    });
    if (typeof path !== "string") return null;
    await invoke<void>("model_export_pack", { path });
    return path;
  },
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

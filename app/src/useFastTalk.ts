import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { errorMessage, fastTalkApi } from "./bridge";
import { modelVerificationView } from "./setupPresentation";
import {
  initialRuntimeStatus,
  initialSnapshot,
  type AudioDeviceInfo,
  type AudioDevices,
  type AudioStatus,
  type EngineSnapshot,
  type ModelProgress,
  type ModelStatus,
  type NativeRuntimeStatus,
  type RuntimeProfileOption,
  type WorkerStatus,
} from "./contracts";

function newestSnapshot(current: EngineSnapshot, next: EngineSnapshot): EngineSnapshot {
  return next.revision >= current.revision ? next : current;
}

function preferredDevice(devices: AudioDeviceInfo[]): string | null {
  return (
    devices.find((device) => device.isDefault && device.isCompatible)?.id ??
    devices.find((device) => device.isCompatible)?.id ??
    null
  );
}

export function workersReady(runtime: NativeRuntimeStatus): boolean {
  return [runtime.llm, runtime.speech, runtime.kokoro].every(
    (worker) => worker?.state === "ready",
  );
}

export function workerList(runtime: NativeRuntimeStatus): WorkerStatus[] {
  return [runtime.llm, runtime.speech, runtime.kokoro].filter(
    (worker): worker is WorkerStatus => worker !== null,
  );
}

export type StartupActivity = {
  phase: "launching" | "warming" | "cancelling";
  startedAt: number;
};

export function useFastTalk() {
  const [snapshot, setSnapshot] = useState(initialSnapshot);
  const [runtime, setRuntime] = useState(initialRuntimeStatus);
  const [audio, setAudio] = useState<AudioStatus | null>(null);
  const [devices, setDevices] = useState<AudioDevices>({ inputs: [], outputs: [] });
  const [models, setModels] = useState<ModelStatus[]>([]);
  const [runtimeProfiles, setRuntimeProfiles] = useState<RuntimeProfileOption[]>([]);
  const [modelProgress, setModelProgress] = useState<ModelProgress | null>(null);
  const [inputDeviceId, setInputDeviceId] = useState<string | null>(null);
  const [outputDeviceId, setOutputDeviceId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [modelsLoading, setModelsLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [startup, setStartup] = useState<StartupActivity | null>(null);
  const modelStatusRequest = useRef<Promise<ModelStatus[]> | null>(null);
  const refreshRequest = useRef<Promise<void> | null>(null);
  const startupCancelled = useRef(false);

  const refreshStatus = useCallback(() => {
    if (refreshRequest.current) return refreshRequest.current;
    const request = (async () => {
      const [runtimeResult, audioResult] = await Promise.allSettled([
        fastTalkApi.runtimeStatus(),
        fastTalkApi.audioStatus(),
      ]);
      if (runtimeResult.status === "fulfilled") {
        setRuntime(runtimeResult.value);
      }
      if (audioResult.status === "fulfilled") {
        setAudio(audioResult.value);
      }
    })().finally(() => {
      refreshRequest.current = null;
    });
    refreshRequest.current = request;
    return request;
  }, []);

  useEffect(() => {
    let disposed = false;
    let removeListener: (() => void) | undefined;
    let removeModelListener: (() => void) | undefined;

    void fastTalkApi
      .onEngineSnapshot((next) => {
        if (!disposed) {
          setSnapshot((current) => newestSnapshot(current, next));
        }
      })
      .then((unlisten) => {
        if (disposed) {
          unlisten();
        } else {
          removeListener = unlisten;
        }
      })
      .catch((cause) => {
        if (!disposed) setError(errorMessage(cause));
      });

    void fastTalkApi
      .onModelProgress((progress) => {
        if (!disposed) setModelProgress(progress);
      })
      .then((unlisten) => {
        if (disposed) unlisten();
        else removeModelListener = unlisten;
      })
      .catch((cause) => {
        if (!disposed) setError(errorMessage(cause));
      });

    void Promise.allSettled([
      fastTalkApi.engineSnapshot(),
      fastTalkApi.runtimeStatus(),
      fastTalkApi.audioStatus(),
      fastTalkApi.audioDevices(),
      fastTalkApi.runtimeProfiles(),
    ]).then(([engineResult, runtimeResult, audioResult, devicesResult, profilesResult]) => {
      if (disposed) return;
      if (engineResult.status === "fulfilled") setSnapshot(engineResult.value);
      if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
      if (audioResult.status === "fulfilled") setAudio(audioResult.value);
      if (devicesResult.status === "fulfilled") {
        setDevices(devicesResult.value);
        setInputDeviceId(preferredDevice(devicesResult.value.inputs));
        setOutputDeviceId(preferredDevice(devicesResult.value.outputs));
      }
      if (profilesResult.status === "fulfilled") setRuntimeProfiles(profilesResult.value);
      const failure = [engineResult, runtimeResult, audioResult, devicesResult, profilesResult].find(
        (result) => result.status === "rejected",
      );
      if (failure?.status === "rejected") setError(errorMessage(failure.reason));
      setLoading(false);
    });

    modelStatusRequest.current ??= fastTalkApi.modelStatus();
    void modelStatusRequest.current
      .then((next) => {
        if (!disposed) setModels(next);
      })
      .catch((cause) => {
        if (!disposed) setError(errorMessage(cause));
      })
      .finally(() => {
        if (!disposed) setModelsLoading(false);
      });

    const interval = window.setInterval(() => void refreshStatus(), 1_000);
    return () => {
      disposed = true;
      removeListener?.();
      removeModelListener?.();
      window.clearInterval(interval);
    };
  }, [refreshStatus]);

  const runAction = useCallback(
    async <T,>(label: string, action: () => Promise<T>, commit?: (value: T) => void) => {
      setBusy(label);
      setError(null);
      try {
        const value = await action();
        commit?.(value);
        return value;
      } catch (cause) {
        setError(errorMessage(cause));
        return null;
      } finally {
        setBusy(null);
      }
    },
    [],
  );

  const selectRuntimeProfile = useCallback(
    (profileId: string) =>
      runAction("Changing language model", async () => {
        const nextRuntime = await fastTalkApi.runtimeSelectProfile(profileId);
        setModelsLoading(true);
        try {
          setModels(await fastTalkApi.modelStatus());
        } finally {
          setModelsLoading(false);
        }
        return nextRuntime;
      }, setRuntime),
    [runAction],
  );

  const prepare = useCallback(async () => {
    if (modelsLoading) return;
    startupCancelled.current = false;
    setBusy("Preparing local services");
    setError(null);
    let currentModels = models;
    if (!modelsReady(currentModels)) {
      try {
        setBusy("Downloading and verifying local models");
        currentModels = await fastTalkApi.modelInstallAll();
        setModels(currentModels);
        setModelProgress(null);
      } catch (cause) {
        setError(errorMessage(cause));
        setStartup(null);
        setBusy(null);
        return;
      }
    }
    const startedAt = Date.now();
    setStartup({ phase: "launching", startedAt });
    setBusy("Starting local services");
    const [runtimeResult, audioResult] = await Promise.allSettled([
      fastTalkApi.runtimeStart(),
      fastTalkApi.audioStart({ inputDeviceId, outputDeviceId }),
    ]);
    if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
    if (audioResult.status === "fulfilled") setAudio(audioResult.value);
    const failures = [runtimeResult, audioResult]
      .filter((result) => result.status === "rejected")
      .map((result) => errorMessage((result as PromiseRejectedResult).reason));
    if (startupCancelled.current) {
      setBusy(null);
      return;
    }
    if (failures.length > 0) {
      const cleanup = await Promise.allSettled([
        runtimeResult.status === "fulfilled" ? fastTalkApi.runtimeStop() : Promise.resolve(null),
        audioResult.status === "fulfilled" ? fastTalkApi.audioStop() : Promise.resolve(),
      ]);
      const stoppedRuntime = cleanup[0];
      if (stoppedRuntime.status === "fulfilled" && stoppedRuntime.value) {
        setRuntime(stoppedRuntime.value);
      }
      if (audioResult.status === "fulfilled") setAudio(null);
      setError(`${failures.join(" ")} Any services that started were stopped. Check diagnostics, then try again.`);
      setStartup(null);
    } else {
      setStartup({ phase: "warming", startedAt });
    }
    setBusy(null);
  }, [inputDeviceId, models, modelsLoading, outputDeviceId]);

  const cancelStartup = useCallback(async () => {
    startupCancelled.current = true;
    setStartup((current) => current && { ...current, phase: "cancelling" });
    setBusy("Cancelling startup");
    const [, , runtimeResult] = await Promise.allSettled([
      fastTalkApi.runtimeCancelStart(),
      fastTalkApi.audioStop(),
      fastTalkApi.runtimeStop(),
    ]);
    if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
    setAudio(null);
    setStartup(null);
    setBusy(null);
  }, []);

  const installModels = useCallback(
    () => runAction("Downloading and verifying local models", fastTalkApi.modelInstallAll, (next) => {
      setModels(next);
      setModelProgress(null);
    }),
    [runAction],
  );

  const importModelPack = useCallback(
    () => runAction("Importing offline model pack", fastTalkApi.modelImportPack, (next) => {
      if (next) setModels(next);
    }),
    [runAction],
  );

  const exportModelPack = useCallback(
    () => runAction("Exporting offline model pack", fastTalkApi.modelExportPack),
    [runAction],
  );

  const restartAudio = useCallback(
    () =>
      runAction("Restarting audio", async () => {
        await fastTalkApi.audioStop();
        return fastTalkApi.audioStart({ inputDeviceId, outputDeviceId });
      }, setAudio),
    [inputDeviceId, outputDeviceId, runAction],
  );

  const startConversation = useCallback(
    () => runAction("Starting conversation", fastTalkApi.conversationStart, (next) => {
      setSnapshot((current) => newestSnapshot(current, next));
    }),
    [runAction],
  );

  const stopConversation = useCallback(
    () => runAction("Stopping conversation", fastTalkApi.conversationStop, setSnapshot),
    [runAction],
  );

  const interrupt = useCallback(
    () => runAction("Interrupting", fastTalkApi.conversationInterrupt),
    [runAction],
  );

  const toggleMute = useCallback(
    () =>
      runAction(
        audio?.muted ? "Unmuting microphone" : "Muting microphone",
        () => fastTalkApi.audioSetMuted(!audio?.muted),
        setAudio,
      ),
    [audio?.muted, runAction],
  );

  const stopServices = useCallback(
    () =>
      runAction("Stopping local services", async () => {
        if (snapshot.state !== "idle") await fastTalkApi.conversationStop();
        await fastTalkApi.audioStop();
        const stopped = await fastTalkApi.runtimeStop();
        setAudio(null);
        setSnapshot(initialSnapshot);
        return stopped;
      }, setRuntime),
    [runAction, snapshot.state],
  );

  const ready = useMemo(
    () => modelsReady(models) && workersReady(runtime) && audio?.active === true,
    [audio, models, runtime],
  );
  const conversationActive = snapshot.state !== "idle";

  useEffect(() => {
    if (!startup || startup.phase !== "warming") return;
    if (workersReady(runtime) && audio?.active === true) {
      setStartup(null);
      return;
    }
    const failed = workerList(runtime).find((worker) => worker.state === "failed");
    if (!failed) return;
    const detail = failed.diagnostics[failed.diagnostics.length - 1]?.message
      ?? "The worker exhausted its restart budget.";
    setError(`${failed.id} could not start. ${detail} Local services were stopped; review diagnostics and try again.`);
    setStartup(null);
    void Promise.allSettled([fastTalkApi.audioStop(), fastTalkApi.runtimeStop()]).then(() => {
      setAudio(null);
      void refreshStatus();
    });
  }, [audio?.active, refreshStatus, runtime, startup]);

  return {
    snapshot,
    runtime,
    audio,
    devices,
    models,
    runtimeProfiles,
    modelProgress,
    inputDeviceId,
    outputDeviceId,
    setInputDeviceId,
    setOutputDeviceId,
    selectRuntimeProfile,
    loading,
    modelsLoading,
    busy,
    error,
    startup,
    ready,
    conversationActive,
    prepare,
    cancelStartup,
    installModels,
    importModelPack,
    exportModelPack,
    restartAudio,
    startConversation,
    stopConversation,
    interrupt,
    toggleMute,
    stopServices,
    refreshStatus,
  };
}

export function modelsReady(models: ModelStatus[]): boolean {
  return modelVerificationView(models, false).ready;
}

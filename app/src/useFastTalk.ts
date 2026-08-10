import { useCallback, useEffect, useMemo, useState } from "react";
import { errorMessage, fastTalkApi } from "./bridge";
import {
  initialRuntimeStatus,
  initialSnapshot,
  type AudioDeviceInfo,
  type AudioDevices,
  type AudioStatus,
  type EngineSnapshot,
  type NativeRuntimeStatus,
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

export function useFastTalk() {
  const [snapshot, setSnapshot] = useState(initialSnapshot);
  const [runtime, setRuntime] = useState(initialRuntimeStatus);
  const [audio, setAudio] = useState<AudioStatus | null>(null);
  const [devices, setDevices] = useState<AudioDevices>({ inputs: [], outputs: [] });
  const [inputDeviceId, setInputDeviceId] = useState<string | null>(null);
  const [outputDeviceId, setOutputDeviceId] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refreshStatus = useCallback(async () => {
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
  }, []);

  useEffect(() => {
    let disposed = false;
    let removeListener: (() => void) | undefined;

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

    void Promise.allSettled([
      fastTalkApi.engineSnapshot(),
      fastTalkApi.runtimeStatus(),
      fastTalkApi.audioStatus(),
      fastTalkApi.audioDevices(),
    ]).then(([engineResult, runtimeResult, audioResult, devicesResult]) => {
      if (disposed) return;
      if (engineResult.status === "fulfilled") setSnapshot(engineResult.value);
      if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
      if (audioResult.status === "fulfilled") setAudio(audioResult.value);
      if (devicesResult.status === "fulfilled") {
        setDevices(devicesResult.value);
        setInputDeviceId(preferredDevice(devicesResult.value.inputs));
        setOutputDeviceId(preferredDevice(devicesResult.value.outputs));
      }
      const failure = [engineResult, runtimeResult, audioResult, devicesResult].find(
        (result) => result.status === "rejected",
      );
      if (failure?.status === "rejected") setError(errorMessage(failure.reason));
      setLoading(false);
    });

    const interval = window.setInterval(() => void refreshStatus(), 1_000);
    return () => {
      disposed = true;
      removeListener?.();
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

  const prepare = useCallback(async () => {
    setBusy("Preparing local services");
    setError(null);
    const [runtimeResult, audioResult] = await Promise.allSettled([
      fastTalkApi.runtimeStart(),
      fastTalkApi.audioStart({ inputDeviceId, outputDeviceId }),
    ]);
    if (runtimeResult.status === "fulfilled") setRuntime(runtimeResult.value);
    if (audioResult.status === "fulfilled") setAudio(audioResult.value);
    const failures = [runtimeResult, audioResult]
      .filter((result) => result.status === "rejected")
      .map((result) => errorMessage((result as PromiseRejectedResult).reason));
    if (failures.length > 0) setError(failures.join(" "));
    setBusy(null);
  }, [inputDeviceId, outputDeviceId]);

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

  const ready = useMemo(() => workersReady(runtime) && audio?.active === true, [audio, runtime]);
  const conversationActive = snapshot.state !== "idle";

  return {
    snapshot,
    runtime,
    audio,
    devices,
    inputDeviceId,
    outputDeviceId,
    setInputDeviceId,
    setOutputDeviceId,
    loading,
    busy,
    error,
    ready,
    conversationActive,
    prepare,
    restartAudio,
    startConversation,
    stopConversation,
    interrupt,
    toggleMute,
    stopServices,
    refreshStatus,
  };
}

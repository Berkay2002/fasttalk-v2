import type {
  AudioDeviceInfo,
  ConversationState,
  NativeRuntimeStatus,
  ModelProgress,
  ModelStatus,
  WorkerState,
  WorkerStatus,
} from "./contracts";
import fastTalkMark from "./assets/fasttalk-mark.png";
import { AgentChatTranscript } from "./components/AgentChatTranscript";
import { AgentWave } from "./components/AgentWave";
import { modelsReady, useFastTalk, workerList, workersReady } from "./useFastTalk";
import "./App.css";

const stateCopy: Record<ConversationState, { label: string; detail: string }> = {
  idle: { label: "Ready", detail: "Start a conversation when setup is complete." },
  listening: { label: "Listening", detail: "Speak naturally. FastTalk will detect the end of your turn." },
  thinking: { label: "Thinking", detail: "The response is being generated locally." },
  speaking: { label: "Speaking", detail: "You can interrupt as soon as you want to respond." },
  interrupted: { label: "Interrupted", detail: "Playback stopped. Returning to listening." },
  faulted: { label: "Needs attention", detail: "Open diagnostics for the failure details." },
};

const workerNames: Record<string, string> = {
  llm: "Language model",
  speech: "Speech engine",
  kokoro: "CPU voice fallback",
};

function App() {
  const fastTalk = useFastTalk();
  const {
    snapshot,
    runtime,
    audio,
    devices,
    models,
    modelProgress,
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
    installModels,
    importModelPack,
    exportModelPack,
    restartAudio,
    startConversation,
    stopConversation,
    interrupt,
    toggleMute,
    stopServices,
  } = fastTalk;
  const copy = stateCopy[snapshot.state];
  const selectedAudioChanged =
    audio !== null &&
    (audio.inputDeviceId !== inputDeviceId || audio.outputDeviceId !== outputDeviceId);
  const localModelsReady = modelsReady(models);

  if (loading) {
    return <LoadingScreen />;
  }

  return (
    <main className="app-shell" data-conversation-state={snapshot.state} aria-busy={busy !== null}>
      <header className="app-header">
        <div className="brand-lockup">
          <img className="brand-mark" src={fastTalkMark} alt="" />
          <div>
            <strong>FastTalk</strong>
          </div>
        </div>
        <ReadinessLabel ready={ready} busy={busy} />
      </header>

      {error && (
        <div className="error-banner" role="alert">
          <strong>FastTalk could not complete that action.</strong>
          <span>{error}</span>
        </div>
      )}

      <div className="workspace-grid">
        <section className="conversation-pane" aria-labelledby="conversation-heading">
          <div className="conversation-state">
            <AgentWave state={snapshot.state} voiceActive={audio?.speechActive ?? false} />
            <div>
              <h1 id="conversation-heading">{copy.label}</h1>
              <p>{snapshot.lastError ?? copy.detail}</p>
            </div>
          </div>

          <AgentChatTranscript snapshot={snapshot} />

          <div className="conversation-controls" aria-label="Conversation controls">
            {!conversationActive ? (
              <button
                className="button button-primary"
                disabled={!ready || busy !== null}
                onClick={() => void startConversation()}
              >
                Start conversation
              </button>
            ) : (
              <button
                className="button button-primary"
                disabled={busy !== null}
                onClick={() => void stopConversation()}
              >
                Stop conversation
              </button>
            )}
            <button
              className="button button-secondary"
              disabled={audio === null || busy !== null}
              aria-pressed={audio?.muted ?? false}
              onClick={() => void toggleMute()}
            >
              {audio?.muted ? "Unmute" : "Mute"}
            </button>
            <button
              className="button button-secondary"
              disabled={!(["thinking", "speaking"] as ConversationState[]).includes(snapshot.state) || busy !== null}
              onClick={() => void interrupt()}
            >
              Interrupt
            </button>
          </div>
          {busy && <p className="busy-label" role="status">{busy}</p>}
        </section>

        <aside className="setup-pane" aria-labelledby="setup-heading">
          <div className="section-heading">
            <div>
              <h2 id="setup-heading">Local setup</h2>
              <p>{setupSummary(runtime, audio !== null, localModelsReady)}</p>
            </div>
            <SetupCount runtime={runtime} audioReady={audio?.active === true} modelsReady={localModelsReady} />
          </div>

          <ModelSetup
            models={models}
            progress={modelProgress}
            busy={busy !== null || conversationActive}
            onInstall={() => void installModels()}
            onImport={() => void importModelPack()}
            onExport={() => void exportModelPack()}
          />

          <div className="worker-list" aria-label="Local service readiness">
            <WorkerRow name="Language model" worker={runtime.llm} />
            <WorkerRow name="Speech engine" worker={runtime.speech} />
            <WorkerRow name="CPU voice fallback" worker={runtime.kokoro} />
            <div className="worker-row">
              <span className={`semantic-state state-${audio?.active ? "ready" : "stopped"}`} />
              <span>Windows audio</span>
              <small>{audio?.active ? "Ready" : "Stopped"}</small>
            </div>
          </div>

          <div className="device-fields">
            <DeviceField
              label="Microphone"
              value={inputDeviceId}
              devices={devices.inputs}
              disabled={conversationActive || busy !== null}
              onChange={setInputDeviceId}
            />
            <DeviceField
              label="Speakers"
              value={outputDeviceId}
              devices={devices.outputs}
              disabled={conversationActive || busy !== null}
              onChange={setOutputDeviceId}
            />
          </div>

          {selectedAudioChanged && (
            <button
              className="text-action"
              disabled={conversationActive || busy !== null}
              onClick={() => void restartAudio()}
            >
              Apply audio changes
            </button>
          )}

          {!ready ? (
            <button
              className="button button-setup"
              disabled={busy !== null || inputDeviceId === null || outputDeviceId === null}
              onClick={() => void prepare()}
            >
              Prepare FastTalk
            </button>
          ) : (
            <button
              className="text-action stop-services"
              disabled={conversationActive || busy !== null}
              onClick={() => void stopServices()}
            >
              Stop local services
            </button>
          )}

          <Diagnostics runtime={runtime} audio={audio} />
        </aside>
      </div>
    </main>
  );
}

function LoadingScreen() {
  return (
    <main className="loading-shell" aria-label="Loading FastTalk">
      <div className="loading-brand" />
      <div className="loading-grid">
        <div className="loading-block loading-main" />
        <div className="loading-block loading-side" />
      </div>
    </main>
  );
}

function ReadinessLabel({ ready, busy }: { ready: boolean; busy: string | null }) {
  const label = busy ?? (ready ? "Ready for conversation" : "Setup required");
  return (
    <div className="readiness-label" role="status">
      <span className={`semantic-state state-${ready ? "ready" : busy ? "starting" : "stopped"}`} />
      {label}
    </div>
  );
}

function SetupCount({
  runtime,
  audioReady,
  modelsReady: modelReady,
}: {
  runtime: NativeRuntimeStatus;
  audioReady: boolean;
  modelsReady: boolean;
}) {
  const count = workerList(runtime).filter((worker) => worker.state === "ready").length
    + Number(audioReady)
    + Number(modelReady);
  return <span className="setup-count" aria-label={`${count} of 5 setup components ready`}>{count}/5 ready</span>;
}

function ModelSetup({
  models,
  progress,
  busy,
  onInstall,
  onImport,
  onExport,
}: {
  models: ModelStatus[];
  progress: ModelProgress | null;
  busy: boolean;
  onInstall: () => void;
  onImport: () => void;
  onExport: () => void;
}) {
  const ready = modelsReady(models);
  const verified = models.filter((model) => model.state === "ready").length;
  const progressPercent = progress && progress.totalBytes > 0
    ? Math.round((progress.downloadedBytes / progress.totalBytes) * 100)
    : null;
  return (
    <section className="model-setup" aria-label="Local model setup">
      <div className="worker-row">
        <span className={`semantic-state state-${ready ? "ready" : progress ? "starting" : "stopped"}`} />
        <span>Local model files</span>
        <small>{ready ? "Verified" : `${verified}/${models.length || 5}`}</small>
      </div>
      {progress && (
        <div className="model-progress" role="status">
          <progress value={progress.downloadedBytes} max={progress.totalBytes} />
          <span>{progress.modelId} {progressPercent}%</span>
        </div>
      )}
      <div className="model-actions">
        {!ready && <button className="text-action" disabled={busy} onClick={onInstall}>Download models</button>}
        <button className="text-action" disabled={busy} onClick={onImport}>Import offline pack</button>
        <button className="text-action" disabled={busy || !ready} onClick={onExport}>Export offline pack</button>
      </div>
      <details className="model-licenses">
        <summary>Model versions and licenses</summary>
        {models.map((model) => (
          <div key={model.id}>
            <span>{model.displayName}</span>
            <small>{model.licenseName}</small>
            <code title={model.licenseUrl}>{model.licenseUrl}</code>
          </div>
        ))}
      </details>
    </section>
  );
}

function WorkerRow({ name, worker }: { name: string; worker: WorkerStatus | null }) {
  const state = worker?.state ?? "stopped";
  return (
    <div className="worker-row">
      <span className={`semantic-state state-${state}`} />
      <span>{name}</span>
      <small>{workerStateLabel(state)}</small>
    </div>
  );
}

function DeviceField({
  label,
  value,
  devices,
  disabled,
  onChange,
}: {
  label: string;
  value: string | null;
  devices: AudioDeviceInfo[];
  disabled: boolean;
  onChange: (value: string | null) => void;
}) {
  return (
    <label className="device-field">
      <span>{label}</span>
      <select value={value ?? ""} disabled={disabled} onChange={(event) => onChange(event.target.value || null)}>
        {devices.length === 0 && <option value="">No compatible device found</option>}
        {devices.map((device) => (
          <option key={device.id} value={device.id} disabled={!device.isCompatible}>
            {device.name}{device.isDefault ? " (Default)" : ""}{!device.isCompatible ? " (Unsupported format)" : ""}
          </option>
        ))}
      </select>
    </label>
  );
}

function Diagnostics({
  runtime,
  audio,
}: {
  runtime: NativeRuntimeStatus;
  audio: ReturnType<typeof useFastTalk>["audio"];
}) {
  const workers = workerList(runtime);
  const lines = workers.flatMap((worker) =>
    worker.diagnostics.slice(-4).map((line) => ({ ...line, worker: workerNames[worker.id] ?? worker.id })),
  );
  return (
    <details className="diagnostics">
      <summary>Diagnostics</summary>
      <div className="diagnostic-metrics">
        <Metric label="Voice backend" value={runtime.ttsBackend === "magpie" ? "Magpie GPU" : "Kokoro CPU"} />
        <Metric
          label="Projected VRAM"
          value={runtime.vramAdmission.projectedWarmedMib == null
            ? "Not measured"
            : `${runtime.vramAdmission.projectedWarmedMib} / ${runtime.vramAdmission.limitMib} MiB`}
        />
        <Metric label="Queued audio" value={audio ? `${audio.queuedPlaybackSamples} samples` : "Unavailable"} />
        <Metric label="Capture drops" value={String(audio?.droppedCaptureSamples ?? 0)} />
        <Metric label="Playback drops" value={String(audio?.droppedPlaybackSamples ?? 0)} />
        <Metric label="ASR drops" value={String(audio?.droppedAsrSamples ?? 0)} />
        <Metric
          label="Last interrupt"
          value={audio?.lastCancelToCallbackMs == null ? "Not measured" : `${audio.lastCancelToCallbackMs.toFixed(1)} ms`}
        />
      </div>
      {lines.length > 0 ? (
        <div className="diagnostic-log" aria-label="Recent native worker diagnostics">
          {lines.map((line, index) => (
            <p key={`${line.worker}-${index}-${line.message}`}>
              <span>{line.worker}</span>
              {line.message}
            </p>
          ))}
        </div>
      ) : (
        <p className="diagnostic-empty">No native worker messages yet.</p>
      )}
    </details>
  );
}

function Metric({ label, value }: { label: string; value: string }) {
  return <div><span>{label}</span><strong>{value}</strong></div>;
}

function workerStateLabel(state: WorkerState): string {
  const labels: Record<WorkerState, string> = {
    stopped: "Stopped",
    starting: "Starting",
    ready: "Ready",
    unhealthy: "Unhealthy",
    restartPending: "Restarting",
    failed: "Failed",
  };
  return labels[state];
}

function setupSummary(
  runtime: NativeRuntimeStatus,
  audioReady: boolean,
  modelReady: boolean,
): string {
  if (modelReady && workersReady(runtime) && audioReady) return "Models and Windows audio are ready.";
  if (!modelReady) return "Download or import the verified local model pack.";
  if (workerList(runtime).some((worker) => worker.state === "starting")) return "Loading local models into memory.";
  return "Choose audio devices, then start the local models.";
}

export default App;

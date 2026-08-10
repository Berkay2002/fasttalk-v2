import type { ReactNode } from "react";
import type {
  AudioDeviceInfo,
  ConversationState,
  EngineSnapshot,
  NativeRuntimeStatus,
  WorkerState,
  WorkerStatus,
} from "./contracts";
import { useFastTalk, workerList, workersReady } from "./useFastTalk";
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
  } = fastTalk;
  const copy = stateCopy[snapshot.state];
  const selectedAudioChanged =
    audio !== null &&
    (audio.inputDeviceId !== inputDeviceId || audio.outputDeviceId !== outputDeviceId);

  if (loading) {
    return <LoadingScreen />;
  }

  return (
    <main className="app-shell" data-conversation-state={snapshot.state} aria-busy={busy !== null}>
      <header className="app-header">
        <div className="brand-lockup">
          <span className="brand-mark" aria-hidden="true">F</span>
          <div>
            <strong>FastTalk</strong>
            <span>Private voice on this PC</span>
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
            <VoiceSignal state={snapshot.state} />
            <div>
              <h1 id="conversation-heading">{copy.label}</h1>
              <p>{snapshot.lastError ?? copy.detail}</p>
            </div>
          </div>

          <Transcript snapshot={snapshot} ready={ready} />

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
              <p>{setupSummary(runtime, audio !== null)}</p>
            </div>
            <SetupCount runtime={runtime} audioReady={audio?.active === true} />
          </div>

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

function VoiceSignal({ state }: { state: ConversationState }) {
  return (
    <div className={`voice-signal signal-${state}`} aria-hidden="true">
      {[0, 1, 2, 3, 4, 5, 6].map((bar) => <span key={bar} />)}
    </div>
  );
}

function Transcript({ snapshot, ready }: { snapshot: EngineSnapshot; ready: boolean }) {
  const currentUser = snapshot.committedTranscript || snapshot.partialTranscript;
  const hasContent =
    snapshot.transcript.length > 0 || currentUser.length > 0 || snapshot.assistantTranscript.length > 0;
  return (
    <div className="transcript" aria-live="polite" aria-relevant="additions text">
      {!hasContent && (
        <div className="transcript-empty">
          <p>{ready ? "Your conversation will appear here." : "Complete local setup to begin."}</p>
          <span>Audio and inference stay on this computer.</span>
        </div>
      )}
      {snapshot.transcript.map((message, index) => (
        <TranscriptLine key={`${index}-${message.role}-${message.text}`} role={message.role}>
          {message.text}
        </TranscriptLine>
      ))}
      {currentUser && (
        <TranscriptLine role="user" live={snapshot.partialTranscript.length > 0}>
          {currentUser}
        </TranscriptLine>
      )}
      {snapshot.assistantTranscript && (
        <TranscriptLine role="assistant" live>
          {snapshot.assistantTranscript}
        </TranscriptLine>
      )}
    </div>
  );
}

function TranscriptLine({
  role,
  live = false,
  children,
}: {
  role: "user" | "assistant";
  live?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`transcript-line transcript-${role}${live ? " transcript-live" : ""}`}>
      <span>{role === "user" ? "You" : "FastTalk"}</span>
      <p>{children}</p>
    </div>
  );
}

function SetupCount({ runtime, audioReady }: { runtime: NativeRuntimeStatus; audioReady: boolean }) {
  const count = workerList(runtime).filter((worker) => worker.state === "ready").length + Number(audioReady);
  return <span className="setup-count" aria-label={`${count} of 4 setup components ready`}>{count}/4 ready</span>;
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

function setupSummary(runtime: NativeRuntimeStatus, audioReady: boolean): string {
  if (workersReady(runtime) && audioReady) return "Models and Windows audio are ready.";
  if (workerList(runtime).some((worker) => worker.state === "starting")) return "Loading local models into memory.";
  return "Choose audio devices, then start the local models.";
}

export default App;

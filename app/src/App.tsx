import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type ConversationState =
  | "idle"
  | "listening"
  | "thinking"
  | "speaking"
  | "interrupted"
  | "faulted";

type EngineSnapshot = {
  state: ConversationState;
  turnId: number;
  revision: number;
  cancellationEpoch: number;
  partialTranscript: string;
  committedTranscript: string;
  pendingClause: string | null;
  lastError: string | null;
};

type EngineEvent =
  | { type: "startListening" }
  | { type: "endOfSpeech"; payload: { transcript: string } }
  | { type: "clauseReady"; payload: { text: string } }
  | { type: "playbackDrained" }
  | { type: "interrupt" }
  | { type: "reset" };

const initialSnapshot: EngineSnapshot = {
  state: "idle",
  turnId: 0,
  revision: 0,
  cancellationEpoch: 0,
  partialTranscript: "",
  committedTranscript: "",
  pendingClause: null,
  lastError: null,
};

function App() {
  const [snapshot, setSnapshot] = useState(initialSnapshot);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<EngineSnapshot>("engine_snapshot").then(setSnapshot).catch((cause) => {
      setError(String(cause));
    });
  }, []);

  async function dispatch(event: EngineEvent) {
    try {
      setSnapshot(await invoke<EngineSnapshot>("engine_dispatch", { event }));
      setError(null);
    } catch (cause) {
      setError(String(cause));
    }
  }

  const primaryAction = (() => {
    switch (snapshot.state) {
      case "idle":
      case "interrupted":
        return { label: "Start listening", event: { type: "startListening" } as EngineEvent };
      case "listening":
        return {
          label: "Finish turn",
          event: {
            type: "endOfSpeech",
            payload: { transcript: "Local speech input is not connected yet." },
          } as EngineEvent,
        };
      case "thinking":
        return {
          label: "Queue test clause",
          event: {
            type: "clauseReady",
            payload: { text: "The conversation engine is ready." },
          } as EngineEvent,
        };
      case "speaking":
        return { label: "Playback drained", event: { type: "playbackDrained" } as EngineEvent };
      case "faulted":
        return { label: "Reset", event: { type: "reset" } as EngineEvent };
    }
  })();

  return (
    <main className="shell">
      <header>
        <p className="eyebrow">FastTalk v2</p>
        <h1>Local voice, without the cloud.</h1>
        <p className="summary">
          The native conversation engine is wired. Audio workers remain disabled until setup is complete.
        </p>
      </header>

      <section className="status-card" aria-live="polite">
        <div>
          <span className={`state-dot state-${snapshot.state}`} />
          <p className="state-label">{snapshot.state}</p>
          <p className="state-detail">Turn {snapshot.turnId} · revision {snapshot.revision}</p>
        </div>
        <div className="actions">
          <button onClick={() => dispatch(primaryAction.event)}>{primaryAction.label}</button>
          <button
            className="secondary"
            disabled={!(["listening", "thinking", "speaking"] as ConversationState[]).includes(snapshot.state)}
            onClick={() => dispatch({ type: "interrupt" })}
          >
            Interrupt
          </button>
        </div>
      </section>

      <section className="transcript-card">
        <p className="section-label">Current turn</p>
        <p>{snapshot.committedTranscript || snapshot.partialTranscript || "Waiting for speech."}</p>
        {snapshot.pendingClause && <blockquote>{snapshot.pendingClause}</blockquote>}
      </section>

      {error && <p className="error">{error}</p>}
    </main>
  );
}

export default App;

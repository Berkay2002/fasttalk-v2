import { useEffect, useRef } from "react";
import type { EngineSnapshot, TranscriptMessage } from "../contracts";

export function AgentChatTranscript({ snapshot }: { snapshot: EngineSnapshot }) {
  const viewportRef = useRef<HTMLDivElement>(null);
  const pinnedToBottomRef = useRef(true);
  const currentUser = snapshot.committedTranscript || snapshot.partialTranscript;
  const hasContent = snapshot.transcript.length > 0
    || currentUser.length > 0
    || snapshot.assistantTranscript.length > 0;

  useEffect(() => {
    const viewport = viewportRef.current;
    if (viewport && pinnedToBottomRef.current) viewport.scrollTop = viewport.scrollHeight;
  }, [snapshot.revision]);

  return (
    <div
      ref={viewportRef}
      className="agent-chat"
      onScroll={(event) => {
        const viewport = event.currentTarget;
        pinnedToBottomRef.current = viewport.scrollHeight - viewport.scrollTop - viewport.clientHeight < 24;
      }}
      role="log"
      aria-live="polite"
      aria-relevant="additions text"
      aria-busy={snapshot.state === "thinking"}
    >
      {!hasContent && snapshot.state !== "thinking" && (
        <p className="agent-chat-empty">The transcript will appear here.</p>
      )}
      {snapshot.transcript.map((message, index) => (
        <ChatMessage key={`${index}-${message.role}-${message.text}`} message={message} />
      ))}
      {currentUser && (
        <ChatMessage
          message={{ role: "user", text: currentUser }}
          streaming={snapshot.partialTranscript.length > 0}
        />
      )}
      {snapshot.state === "thinking" && !snapshot.assistantTranscript && <ThinkingIndicator />}
      {snapshot.assistantTranscript && (
        <ChatMessage message={{ role: "assistant", text: snapshot.assistantTranscript }} streaming />
      )}
    </div>
  );
}

function ChatMessage({ message, streaming = false }: { message: TranscriptMessage; streaming?: boolean }) {
  return (
    <div className={`agent-message message-${message.role}${streaming ? " message-streaming" : ""}`}>
      <span>{message.role === "user" ? "You" : "FastTalk"}</span>
      <p>{message.text}</p>
    </div>
  );
}

function ThinkingIndicator() {
  return (
    <div className="agent-thinking" role="status" aria-label="FastTalk is thinking">
      <span />
      <span />
      <span />
    </div>
  );
}

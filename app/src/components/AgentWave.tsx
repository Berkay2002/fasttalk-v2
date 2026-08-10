import { useEffect, useRef } from "react";
import type { ConversationState } from "../contracts";

type AgentWaveProps = {
  state: ConversationState;
  voiceActive: boolean;
};

const stateMotion: Record<ConversationState, { amplitude: number; frequency: number; speed: number; opacity: number }> = {
  idle: { amplitude: 0, frequency: 1, speed: 0, opacity: 0.22 },
  listening: { amplitude: 0.1, frequency: 2.1, speed: 1.25, opacity: 0.72 },
  thinking: { amplitude: 0.035, frequency: 5.4, speed: 3.4, opacity: 0.58 },
  speaking: { amplitude: 0.18, frequency: 3.2, speed: 2.2, opacity: 1 },
  interrupted: { amplitude: 0.025, frequency: 1.4, speed: 0.6, opacity: 0.4 },
  faulted: { amplitude: 0, frequency: 1, speed: 0, opacity: 0.35 },
};

export function AgentWave({ state, voiceActive }: AgentWaveProps) {
  const canvasRef = useRef<HTMLCanvasElement>(null);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;
    const context = canvas.getContext("2d");
    if (!context) return;

    let frame = 0;
    let animation = 0;
    const reducedMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches;
    const resize = () => {
      const bounds = canvas.getBoundingClientRect();
      const ratio = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.max(1, Math.round(bounds.width * ratio));
      canvas.height = Math.max(1, Math.round(bounds.height * ratio));
      context.setTransform(ratio, 0, 0, ratio, 0, 0);
    };
    const observer = new ResizeObserver(resize);
    observer.observe(canvas);
    resize();

    const draw = () => {
      const width = canvas.clientWidth;
      const height = canvas.clientHeight;
      const motion = stateMotion[state];
      const activity = state === "listening" && !voiceActive ? 0.38 : 1;
      context.clearRect(0, 0, width, height);
      context.beginPath();
      for (let x = 0; x <= width; x += 1.5) {
        const normalized = x / Math.max(width, 1);
        const distance = Math.abs(normalized - 0.5) * 2;
        const envelope = Math.cos(Math.min(1, distance) * Math.PI * 0.5) ** 3;
        const wave = Math.sin(normalized * Math.PI * 2 * motion.frequency + frame * motion.speed);
        const y = height * 0.5 + wave * height * motion.amplitude * envelope * activity;
        if (x === 0) context.moveTo(x, y);
        else context.lineTo(x, y);
      }
      context.lineWidth = 1.6;
      context.lineCap = "round";
      context.strokeStyle = `rgba(183, 231, 194, ${motion.opacity})`;
      context.stroke();
      frame += reducedMotion ? 0 : 0.025;
      if (!reducedMotion) animation = requestAnimationFrame(draw);
    };
    draw();
    return () => {
      observer.disconnect();
      cancelAnimationFrame(animation);
    };
  }, [state, voiceActive]);

  return <canvas ref={canvasRef} className="agent-wave" aria-hidden="true" />;
}

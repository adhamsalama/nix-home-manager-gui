import { useEffect, useRef, useState } from "react";
import { onRunDone, onRunLog, type LogLine, type RunDone } from "../api";

export interface RunState {
  runId: string | null;
  lines: LogLine[];
  done: RunDone | null;
}

/**
 * Tracks one streamed run at a time. Events can arrive before `start`
 * resolves with the run id, so every event is buffered and filtered by id.
 */
export function useRun(onFinished?: (done: RunDone) => void) {
  const [runId, setRunId] = useState<string | null>(null);
  const [lines, setLines] = useState<LogLine[]>([]);
  const [results, setResults] = useState<Record<string, RunDone>>({});
  const finishedRef = useRef(onFinished);
  finishedRef.current = onFinished;

  useEffect(() => {
    const unlisten = [
      onRunLog((l) => setLines((prev) => [...prev, l])),
      onRunDone((d) => {
        setResults((prev) => ({ ...prev, [d.runId]: d }));
        finishedRef.current?.(d);
      }),
    ];
    return () => unlisten.forEach((p) => p.then((f) => f()));
  }, []);

  const start = async (launch: () => Promise<string>) => {
    setLines([]);
    setRunId(null);
    const id = await launch();
    setRunId(id);
    return id;
  };

  const state: RunState = {
    runId,
    lines: runId ? lines.filter((l) => l.runId === runId) : [],
    done: runId ? (results[runId] ?? null) : null,
  };
  return { ...state, running: runId !== null && !state.done, start };
}

export function RunLog({ run, title }: { run: RunState; title: string }) {
  const bottom = useRef<HTMLDivElement>(null);
  useEffect(() => bottom.current?.scrollIntoView({ block: "nearest" }), [run.lines.length]);

  if (!run.runId) return null;
  return (
    <section className="run-log">
      <header>
        <span>{title}</span>
        {run.done ? (
          <span className={run.done.success ? "badge ok" : "badge bad"}>
            {run.done.success ? "Succeeded" : `Failed${run.done.code !== null ? ` (exit ${run.done.code})` : ""}`}
          </span>
        ) : (
          <span className="badge running">Running…</span>
        )}
      </header>
      <pre>
        {run.lines.map((l, i) => (
          <div key={i} className={`log-${l.stream}`}>
            {l.line}
          </div>
        ))}
        <div ref={bottom} />
      </pre>
    </section>
  );
}

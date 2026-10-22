import { useEffect, useState } from "react";
import { api, errorText, type Changes } from "../api";
import { RunLog, useRun } from "../components/RunLog";

export function ApplyPage({ revision, onChanged }: { revision: number; onChanged: () => void }) {
  const [changes, setChanges] = useState<Changes | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [lastAction, setLastAction] = useState("build");
  const run = useRun(() => onChanged());

  useEffect(() => {
    api
      .getChanges()
      .then(setChanges)
      .catch((e) => setError(errorText(e)));
  }, [revision]);

  const act = async (f: () => Promise<unknown>) => {
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const start = (action: "build" | "switch") =>
    act(async () => {
      setLastAction(action);
      await run.start(() => api.startRun(action));
    });

  return (
    <div className="page">
      <h2>Review & apply</h2>
      {changes && <p className="muted">{changes.path}</p>}
      {error && <pre className="error-block">{error}</pre>}

      {changes?.dirty ? (
        <>
          <div className="diff">
            {changes.diff.map((l, i) => (
              <div key={i} className={`diff-${l.kind}`}>
                <span className="ln">{l.kind === "hunk" ? "" : (l.oldLine ?? "")}</span>
                <span className="ln">{l.kind === "hunk" ? "" : (l.newLine ?? "")}</span>
                <span className="sign">{l.kind === "add" ? "+" : l.kind === "del" ? "-" : " "}</span>
                <span className="text">{l.text}</span>
              </div>
            ))}
          </div>
          <div className="row">
            <button className="primary" onClick={() => act(async () => (await api.saveChanges(), onChanged()))}>
              Save to file
            </button>
            <button className="danger" onClick={() => act(async () => (await api.discardChanges(), onChanged()))}>
              Discard changes
            </button>
          </div>
          <p className="muted small">Saving keeps a copy of the previous file next to it as <code>.nix.bak</code>.</p>
        </>
      ) : (
        changes && <p className="notice">No unsaved changes. The file matches what's on disk.</p>
      )}

      <h3>Apply with home-manager</h3>
      <p className="muted">
        <b>Build</b> checks that the configuration evaluates and builds without changing anything.{" "}
        <b>Switch</b> builds it and activates it as a new generation.
      </p>
      <div className="row">
        <button disabled={run.running || changes?.dirty} onClick={() => start("build")}>
          Build
        </button>
        <button className="primary" disabled={run.running || changes?.dirty} onClick={() => start("switch")}>
          Switch
        </button>
        {changes?.dirty && <span className="muted small">Save your changes first.</span>}
      </div>
      <RunLog run={run} title={`home-manager ${lastAction}`} />
    </div>
  );
}

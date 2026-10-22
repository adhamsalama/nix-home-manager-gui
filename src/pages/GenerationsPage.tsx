import { useEffect, useState } from "react";
import { api, errorText, type Generation } from "../api";
import { RunLog, useRun } from "../components/RunLog";

export function GenerationsPage({ revision }: { revision: number }) {
  const [generations, setGenerations] = useState<Generation[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<number | null>(null);
  const [activating, setActivating] = useState<number | null>(null);

  const load = () =>
    api
      .listGenerations()
      .then((g) => {
        setGenerations(g);
        setError(null);
      })
      .catch((e) => setError(errorText(e)));

  const run = useRun(() => load());
  useEffect(() => void load(), [revision]);

  const activate = async (id: number) => {
    setConfirming(null);
    setActivating(id);
    setError(null);
    try {
      await run.start(() => api.activateGeneration(id));
    } catch (e) {
      setError(errorText(e));
    }
  };

  return (
    <div className="page">
      <h2>Generations</h2>
      <p className="muted">
        Every switch creates a generation. Activating an older one rolls your environment back to it; your config file
        isn't changed.
      </p>
      {error && <pre className="error-block">{error}</pre>}
      {!generations && !error && <div className="spinner" />}
      {generations && (
        <table className="results">
          <thead>
            <tr>
              <th>ID</th>
              <th>Created</th>
              <th>Store path</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {generations.map((g) => (
              <tr key={g.id} className={g.current ? "current" : ""}>
                <td>{g.id}</td>
                <td className="nowrap">{g.date}</td>
                <td className="muted path" title={g.path}>
                  {g.path}
                </td>
                <td className="nowrap">
                  {g.current ? (
                    <span className="badge ok">Current</span>
                  ) : confirming === g.id ? (
                    <span className="row">
                      <button className="primary" onClick={() => activate(g.id)}>
                        Activate {g.id}
                      </button>
                      <button onClick={() => setConfirming(null)}>Cancel</button>
                    </span>
                  ) : (
                    <button disabled={run.running} onClick={() => setConfirming(g.id)}>
                      Roll back to this
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <RunLog run={run} title={`Activating generation ${activating ?? ""}`} />
    </div>
  );
}

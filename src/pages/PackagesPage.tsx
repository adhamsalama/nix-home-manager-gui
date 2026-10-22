import { useEffect, useState } from "react";
import { api, errorText, type PackageInfo, type Packages } from "../api";

export function PackagesPage({ revision, onChanged }: { revision: number; onChanged: () => void }) {
  const [packages, setPackages] = useState<Packages | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<PackageInfo[] | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .getPackages()
      .then((p) => {
        setPackages(p);
        setListError(null);
      })
      .catch((e) => setListError(errorText(e)));
  }, [revision]);

  const search = async () => {
    setSearching(true);
    setError(null);
    try {
      setResults(await api.searchPackages(query));
    } catch (e) {
      setError(errorText(e));
    } finally {
      setSearching(false);
    }
  };

  const change = async (f: () => Promise<void>) => {
    setError(null);
    try {
      await f();
      onChanged();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const installed = new Set(packages?.items ?? []);

  return (
    <div className="split">
      <aside className="list-pane">
        <div className="list-tools">
          <h3>home.packages</h3>
          {packages && !packages.defined && <p className="muted">Not set yet. Adding a package creates it.</p>}
          {listError && <p className="error">{listError}</p>}
        </div>
        <ul className="package-list">
          {packages?.items.map((name) => (
            <li key={name}>
              <span className="option-name">{name}</span>
              <button className="icon" title={`Remove ${name}`} onClick={() => change(() => api.stageRemovePackage(name))}>
                ×
              </button>
            </li>
          ))}
        </ul>
      </aside>
      <section className="detail-pane">
        <div className="detail">
          <h2>Find packages</h2>
          <form
            className="row"
            onSubmit={(e) => {
              e.preventDefault();
              search();
            }}
          >
            <input type="search" placeholder="Search nixpkgs, e.g. ripgrep" value={query} onChange={(e) => setQuery(e.target.value)} />
            <button className="primary" disabled={searching || query.trim().length < 2}>
              {searching ? "Searching…" : "Search"}
            </button>
          </form>
          {searching && <p className="muted">Searching nixpkgs. The first search can take a while.</p>}
          {error && <p className="error">{error}</p>}
          {results && (
            <table className="results">
              <tbody>
                {results.length === 0 && (
                  <tr>
                    <td className="muted">No packages found.</td>
                  </tr>
                )}
                {results.slice(0, 200).map((p) => (
                  <tr key={p.attr}>
                    <td>
                      <div className="option-name">{p.attr}</div>
                      <div className="muted small">{p.description}</div>
                    </td>
                    <td className="muted nowrap">{p.version}</td>
                    <td className="nowrap">
                      {installed.has(p.attr) ? (
                        <span className="badge ok">Added</span>
                      ) : (
                        <button disabled={!!listError} onClick={() => change(() => api.stageAddPackage(p.attr))}>
                          Add
                        </button>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </section>
    </div>
  );
}

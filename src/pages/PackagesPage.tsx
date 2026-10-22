import { useEffect, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { api, errorText, type HmOption, type PackageDetails, type Packages, type SearchResults } from "../api";
import { relatedOptions } from "../packageOptions";

interface Props {
  revision: number;
  onChanged: () => void;
  onOpenOption: (name: string) => void;
}

export function PackagesPage({ revision, onChanged, onOpenOption }: Props) {
  const [packages, setPackages] = useState<Packages | null>(null);
  const [listError, setListError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SearchResults | null>(null);
  const [searching, setSearching] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<string | null>(null);

  useEffect(() => {
    api
      .getPackages()
      .then((p) => {
        setPackages(p);
        setListError(null);
        // Close the details of a package that was just removed.
        setSelected((s) => (s && p.items.includes(s) ? s : null));
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
        <ul className="option-list">
          {packages?.items.map((name) => (
            <li key={name}>
              <button className={selected === name ? "selected" : ""} onClick={() => setSelected(name)}>
                <span className="option-name">{name}</span>
              </button>
            </li>
          ))}
        </ul>
      </aside>
      <section className="detail-pane">
        {selected ? (
          <PackageDetail
            key={selected}
            attr={selected}
            revision={revision}
            onBack={() => setSelected(null)}
            onRemove={() => change(() => api.stageRemovePackage(selected))}
            onOpenOption={onOpenOption}
          />
        ) : (
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
              <p className="muted small">
                Searched {results.source}: {results.packages.length.toLocaleString()} result
                {results.packages.length === 1 ? "" : "s"}
                {results.packages.length > 200 && ", showing the first 200"}.
              </p>
            )}
            {results?.packages.length === 0 && (
              <div className="notice">
                <p>No packages found.</p>
                <p className="muted small">
                  If you expected results, nixpkgs may have failed to evaluate: <code>nix search</code> hides those
                  errors. To see the real error, run <code>nix eval nixpkgs#hello.name --no-eval-cache</code> in a
                  terminal.
                </p>
              </div>
            )}
            {results && results.packages.length > 0 && (
              <table className="results">
                <tbody>
                  {results.packages.slice(0, 200).map((p) => (
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
        )}
      </section>
    </div>
  );
}

function PackageDetail(props: {
  attr: string;
  revision: number;
  onBack: () => void;
  onRemove: () => void;
  onOpenOption: (name: string) => void;
}) {
  const { attr } = props;
  const [details, setDetails] = useState<PackageDetails | null>(null);
  const [detailsError, setDetailsError] = useState<string | null>(null);
  const [options, setOptions] = useState<HmOption[] | null>(null);
  const [optionsError, setOptionsError] = useState<string | null>(null);
  const [definedPaths, setDefinedPaths] = useState<Set<string>>(new Set());

  useEffect(() => {
    api
      .packageDetails(attr)
      .then(setDetails)
      .catch((e) => setDetailsError(errorText(e)));
    // Already cached by the backend once the Options page has loaded them.
    api
      .loadOptions()
      .then(setOptions)
      .catch((e) => setOptionsError(errorText(e)));
  }, [attr]);

  useEffect(() => {
    api
      .getFileOverview()
      .then((o) => setDefinedPaths(new Set(o.definedPaths)))
      .catch(() => setDefinedPaths(new Set()));
  }, [props.revision]);

  const related = options ? relatedOptions(options, attr) : null;

  return (
    <div className="detail">
      <button className="link" onClick={props.onBack}>
        ← Back to search
      </button>
      <h2 className="option-title">{attr}</h2>

      {detailsError ? (
        <pre className="error-block">{detailsError}</pre>
      ) : !details ? (
        <p className="muted">Looking up the package…</p>
      ) : (
        <>
          <div className="meta">
            {details.version && <span className="type">{details.version}</span>}
            <span className="muted small">from {details.source}</span>
          </div>
          {details.description ? <p>{details.description}</p> : <p className="muted">No description.</p>}
          <dl className="facts">
            {details.homepage && (
              <>
                <dt>Homepage</dt>
                <dd>
                  <button className="link" onClick={() => openUrl(details.homepage!)}>
                    {details.homepage}
                  </button>
                </dd>
              </>
            )}
            {details.licenses.length > 0 && (
              <>
                <dt>License</dt>
                <dd>{details.licenses.join(", ")}</dd>
              </>
            )}
          </dl>
        </>
      )}

      <div className="row actions">
        <button className="danger" onClick={props.onRemove}>
          Remove from home.packages
        </button>
      </div>

      <h3>Configuration options</h3>
      {optionsError ? (
        <p className="error">Couldn't load home-manager options: {optionsError}</p>
      ) : !related ? (
        <div className="spinner small" />
      ) : related.options.length === 0 ? (
        <p className="muted">
          home-manager has no module for this package, so it's configured outside home-manager (or through{" "}
          <code>home.file</code> / <code>xdg.configFile</code>).
        </p>
      ) : (
        <>
          <p className="notice">
            home-manager has a module for this: {related.modules.map((m, i) => (
              <span key={m}>
                {i > 0 && ", "}
                <code>{m}</code>
              </span>
            ))}
            . Enabling it installs the package and manages its configuration, so you won't need it in{" "}
            <code>home.packages</code> as well.
          </p>
          <ul className="option-list inline-list">
            {related.options.map((o) => (
              <li key={o.name}>
                <button onClick={() => props.onOpenOption(o.name)}>
                  <span className="option-name">{o.name}</span>
                  <span className="row">
                    <span className="muted small">{o.type}</span>
                    {definedPaths.has(o.name) && <span className="dot" title="Set in your file" />}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        </>
      )}
    </div>
  );
}

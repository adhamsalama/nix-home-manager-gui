import { useEffect, useMemo, useState } from "react";
import { api, errorText, type FileValue, type HmOption } from "../api";
import { Description } from "../components/Description";
import { OptionEditor } from "../components/OptionEditor";

const MAX_RESULTS = 300;

const isPlaceholder = (part: string) => part === "*" || /^<.*>$/.test(part);

function search(options: HmOption[], query: string, setPaths: Set<string> | null): HmOption[] {
  const terms = query.toLowerCase().split(/\s+/).filter(Boolean);
  const scored: [number, HmOption][] = [];
  for (const o of options) {
    if (setPaths && !setPaths.has(o.name)) continue;
    const name = o.name.toLowerCase();
    const desc = o.description.toLowerCase();
    let score = 0;
    let ok = true;
    for (const t of terms) {
      if (name.startsWith(t)) score += 3;
      else if (name.includes(t)) score += 2;
      else if (desc.includes(t)) score += 1;
      else {
        ok = false;
        break;
      }
    }
    if (ok) scored.push([score, o]);
  }
  // Stable sort keeps alphabetical order within a score.
  return scored.sort((a, b) => b[0] - a[0]).map(([, o]) => o);
}

export interface OptionFocus {
  name: string;
  /** Changes on every request, so the same option can be focused twice. */
  nonce: number;
}

interface Props {
  revision: number;
  onChanged: () => void;
  focus: OptionFocus | null;
}

export function OptionsPage({ revision, onChanged, focus }: Props) {
  const [options, setOptions] = useState<HmOption[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [onlySet, setOnlySet] = useState(false);
  const [definedPaths, setDefinedPaths] = useState<Set<string>>(new Set());
  const [selected, setSelected] = useState<HmOption | null>(null);

  const load = (refresh: boolean) => {
    setOptions(null);
    setLoadError(null);
    api
      .loadOptions(refresh)
      .then(setOptions)
      .catch((e) => setLoadError(errorText(e)));
  };
  useEffect(() => load(false), []);

  useEffect(() => {
    const option = focus && options?.find((o) => o.name === focus.name);
    if (!option) return;
    setQuery(option.name);
    setOnlySet(false);
    setSelected(option);
  }, [focus?.nonce, options]);

  useEffect(() => {
    api
      .getFileOverview()
      .then((o) => setDefinedPaths(new Set(o.definedPaths)))
      .catch(() => setDefinedPaths(new Set()));
  }, [revision]);

  const results = useMemo(
    () => (options ? search(options, query, onlySet ? definedPaths : null) : []),
    [options, query, onlySet, definedPaths],
  );

  if (loadError)
    return (
      <div className="page centered">
        <h2>Couldn't load home-manager options</h2>
        <pre className="error-block">{loadError}</pre>
        <p className="muted">Check the options source in Settings, or switch to mock mode.</p>
        <button onClick={() => load(true)}>Try again</button>
      </div>
    );
  if (!options)
    return (
      <div className="page centered">
        <div className="spinner" />
        <p className="muted">Loading options… The first load in real mode builds home-manager's docs, which can take a minute.</p>
      </div>
    );

  return (
    <div className="split">
      <aside className="list-pane">
        <div className="list-tools">
          <input
            type="search"
            placeholder={`Search ${options.length.toLocaleString()} options`}
            value={query}
            autoFocus
            onChange={(e) => setQuery(e.target.value)}
          />
          <label className="checkbox">
            <input type="checkbox" checked={onlySet} onChange={(e) => setOnlySet(e.target.checked)} />
            Only options set in my file
          </label>
        </div>
        <ul className="option-list">
          {results.slice(0, MAX_RESULTS).map((o) => (
            <li key={o.name}>
              <button className={selected?.name === o.name ? "selected" : ""} onClick={() => setSelected(o)}>
                <span className="option-name">{o.name}</span>
                {definedPaths.has(o.name) && <span className="dot" title="Set in your file" />}
              </button>
            </li>
          ))}
          {results.length > MAX_RESULTS && (
            <li className="muted more">{(results.length - MAX_RESULTS).toLocaleString()} more; refine your search</li>
          )}
          {results.length === 0 && <li className="muted more">No matching options</li>}
        </ul>
      </aside>
      <section className="detail-pane">
        {selected ? (
          <OptionDetail key={selected.name} option={selected} revision={revision} onChanged={onChanged} />
        ) : (
          <div className="centered muted">Select an option to see its details and set it.</div>
        )}
      </section>
    </div>
  );
}

function OptionDetail({ option, revision, onChanged }: { option: HmOption; revision: number; onChanged: () => void }) {
  const [fills, setFills] = useState<Record<number, string>>({});
  const loc = option.loc.map((p, i) => (isPlaceholder(p) ? (fills[i] ?? "") : p));
  const complete = loc.every((p) => p !== "");
  const locKey = JSON.stringify(loc);

  const [fileValue, setFileValue] = useState<FileValue | null>(null);
  const [fileError, setFileError] = useState<string | null>(null);
  const [evaluated, setEvaluated] = useState<{ value?: string; error?: string; busy?: boolean }>({});

  useEffect(() => {
    setFileValue(null);
    setFileError(null);
    if (!complete) return;
    api
      .getOptionInFile(loc)
      .then(setFileValue)
      .catch((e) => setFileError(errorText(e)));
  }, [locKey, complete, revision]);

  const evaluate = () => {
    setEvaluated({ busy: true });
    api
      .evalOption(loc, option.default)
      .then((value) => setEvaluated({ value }))
      .catch((e) => setEvaluated({ error: errorText(e) }));
  };

  return (
    <div className="detail">
      <h2 className="option-title">{option.name}</h2>
      <div className="meta">
        <span className="type">{option.type}</span>
        {fileValue?.state === "set" && <span className="badge ok">Set on line {fileValue.line}</span>}
      </div>
      <Description text={option.description} />

      <dl className="facts">
        {option.default !== null && (
          <>
            <dt>Default</dt>
            <dd>
              <pre className="code">{option.default}</pre>
            </dd>
          </>
        )}
        {option.example !== null && (
          <>
            <dt>Example</dt>
            <dd>
              <pre className="code">{option.example}</pre>
            </dd>
          </>
        )}
        {option.declarations.length > 0 && (
          <>
            <dt>Declared in</dt>
            <dd className="muted">{option.declarations.join(", ")}</dd>
          </>
        )}
      </dl>

      <h3>In your file</h3>
      {option.loc.some(isPlaceholder) && (
        <div className="placeholders">
          {option.loc.map((p, i) =>
            isPlaceholder(p) ? (
              <label key={i}>
                <span>
                  {option.loc.slice(0, i).join(".")}.<b>{p}</b>
                </span>
                <input
                  type="text"
                  placeholder="e.g. foo.conf"
                  value={fills[i] ?? ""}
                  onChange={(e) => setFills({ ...fills, [i]: e.target.value })}
                />
              </label>
            ) : null,
          )}
        </div>
      )}
      {!complete ? (
        <p className="muted">Fill in the names above to edit this option.</p>
      ) : fileError ? (
        <p className="error">{fileError}</p>
      ) : fileValue ? (
        <OptionEditor
          key={`${locKey}|${fileValue.state === "set" ? fileValue.expr : fileValue.state}`}
          option={option}
          loc={loc}
          fileValue={fileValue}
          onStaged={onChanged}
        />
      ) : (
        <div className="spinner small" />
      )}

      <h3>Effective value</h3>
      <p className="muted">The value after merging every module, including defaults and other files. Staged changes are only included once saved.</p>
      <button disabled={!complete || evaluated.busy} onClick={evaluate}>
        {evaluated.busy ? "Evaluating…" : "Evaluate"}
      </button>
      {evaluated.value !== undefined && <pre className="code">{evaluated.value}</pre>}
      {evaluated.error && <pre className="error-block">{evaluated.error}</pre>}
    </div>
  );
}

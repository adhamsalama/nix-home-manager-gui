import { useState } from "react";
import { api, errorText, type FileValue, type HmOption, type StagedValue } from "../api";

type Widget =
  | { kind: "bool"; nullable: boolean }
  | { kind: "string"; nullable: boolean; multiline: boolean }
  | { kind: "number"; nullable: boolean; integer: boolean }
  | { kind: "enum"; nullable: boolean; values: string[] }
  | { kind: "stringList" }
  | { kind: "stringMap" }
  | { kind: "raw" };

type FormValue = boolean | string | number | null | string[] | [string, string][];

/** Picks a form widget from a home-manager type description. */
export function widgetFor(type: string): Widget {
  const nullable = type.startsWith("null or ");
  const t = nullable ? type.slice("null or ".length) : type;
  if (t === "boolean") return { kind: "bool", nullable };
  if (t.startsWith("strings concatenated with")) return { kind: "string", nullable, multiline: true };
  if (["string", "absolute path", "path", "non-empty string", "string or path"].includes(t))
    return { kind: "string", nullable, multiline: false };
  if (t.startsWith("one of ")) {
    const values = [...t.matchAll(/"((?:[^"\\]|\\.)*)"/g)].map((m) => m[1]);
    return { kind: "enum", nullable, values };
  }
  if (!/\bor\b|list|attribute/.test(t) && /integer/.test(t)) return { kind: "number", nullable, integer: true };
  if (t === "floating point number" || t === "integer or floating point number")
    return { kind: "number", nullable, integer: false };
  if (!nullable && t === "list of string") return { kind: "stringList" };
  if (!nullable && /^(lazy )?attribute set of string$/.test(t)) return { kind: "stringMap" };
  return { kind: "raw" };
}

const STRING = String.raw`"((?:[^"\\$]|\\.|\$(?!\{))*)"`;

function unescapeString(body: string): string {
  return body.replace(/\\(.)/g, (_, c: string) => ({ n: "\n", t: "\t", r: "\r" })[c] ?? c);
}

function parseIndentedString(expr: string): string | undefined {
  const m = expr.match(/^''([\s\S]*)''$/);
  if (!m || /(?<!'')\$\{/.test(m[1])) return undefined;
  let lines = m[1].split("\n");
  if (lines[0].trim() === "") lines = lines.slice(1);
  const last = lines[lines.length - 1];
  if (last !== undefined && last.trim() === "") lines[lines.length - 1] = "";
  const indents = lines.filter((l) => l.trim() !== "").map((l) => l.match(/^ */)![0].length);
  const strip = indents.length ? Math.min(...indents) : 0;
  return lines
    .map((l) => l.slice(strip))
    .join("\n")
    .replace(/'''/g, "''")
    .replace(/''\$/g, "$");
}

/** Reads a Nix expression back into a form value, or undefined if the form can't show it. */
function parseExpr(widget: Widget, expr: string): FormValue | undefined {
  const e = expr.trim();
  if ("nullable" in widget && widget.nullable && e === "null") return null;
  switch (widget.kind) {
    case "bool":
      return e === "true" ? true : e === "false" ? false : undefined;
    case "number":
      return /^-?\d+(\.\d+)?$/.test(e) ? Number(e) : undefined;
    case "string":
    case "enum": {
      const m = e.match(new RegExp(`^${STRING}$`));
      const s = m ? unescapeString(m[1]) : widget.kind === "string" ? parseIndentedString(e) : undefined;
      if (widget.kind === "enum" && s !== undefined && !widget.values.includes(s)) return undefined;
      return s;
    }
    case "stringList": {
      if (!new RegExp(String.raw`^\[\s*(?:${STRING}\s*)*\]$`).test(e)) return undefined;
      return [...e.matchAll(new RegExp(STRING, "g"))].map((m) => unescapeString(m[1]));
    }
    case "stringMap": {
      const entry = String.raw`([A-Za-z_][\w'-]*|${STRING})\s*=\s*${STRING}\s*;`;
      if (!new RegExp(String.raw`^\{\s*(?:${entry}\s*)*\}$`).test(e)) return undefined;
      return [...e.matchAll(new RegExp(entry, "g"))].map(
        (m): [string, string] => [m[2] !== undefined ? unescapeString(m[2]) : m[1], unescapeString(m[3])],
      );
    }
    case "raw":
      return undefined;
  }
}

function emptyValue(widget: Widget): FormValue {
  switch (widget.kind) {
    case "bool":
      return widget.nullable ? null : false;
    case "string":
      return widget.nullable ? null : "";
    case "number":
      return widget.nullable ? null : 0;
    case "enum":
      return widget.nullable ? null : (widget.values[0] ?? "");
    case "stringList":
      return [];
    case "stringMap":
      return [];
    case "raw":
      return null;
  }
}

function toJson(widget: Widget, value: FormValue): unknown {
  if (widget.kind === "stringMap") return Object.fromEntries((value as [string, string][]).filter(([k]) => k.trim()));
  if (widget.kind === "stringList") return (value as string[]).filter((s) => s !== "");
  return value;
}

interface Props {
  option: HmOption;
  loc: string[];
  fileValue: FileValue | null;
  onStaged: () => void;
}

export function OptionEditor({ option, loc, fileValue, onStaged }: Props) {
  const widget = widgetFor(option.type);
  const fileExpr = fileValue?.state === "set" ? fileValue.expr : null;
  const parsedFile = fileExpr !== null ? parseExpr(widget, fileExpr) : undefined;
  const initial =
    parsedFile ?? (option.default !== null ? parseExpr(widget, option.default) : undefined) ?? emptyValue(widget);

  const [raw, setRaw] = useState(widget.kind === "raw" || (fileExpr !== null && parsedFile === undefined));
  const [value, setValue] = useState<FormValue>(initial);
  const [expr, setExpr] = useState(fileExpr ?? option.example ?? option.default ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  if (option.readOnly) return <p className="muted">This option is read-only and can't be set.</p>;
  if (fileValue?.state === "partial")
    return (
      <p className="notice">
        This option is set through its sub-options (line {fileValue.line}). Edit those individually.
      </p>
    );
  if (fileValue?.state === "opaque")
    return (
      <div className="notice">
        <p>This option sits inside an expression the GUI can't edit safely (line {fileValue.line}):</p>
        <pre className="code">{fileValue.expr}</pre>
      </div>
    );

  const act = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
      onStaged();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const stage = () => {
    const staged: StagedValue = raw ? { kind: "raw", expr } : { kind: "json", value: toJson(widget, value) };
    return act(() => api.stageOption(loc, staged));
  };

  return (
    <div className="editor">
      <div className="editor-tabs">
        <button className={!raw ? "tab active" : "tab"} disabled={widget.kind === "raw"} onClick={() => setRaw(false)}>
          Form
        </button>
        <button className={raw ? "tab active" : "tab"} onClick={() => setRaw(true)}>
          Nix expression
        </button>
      </div>

      {raw ? (
        <textarea
          className="code-input"
          spellCheck={false}
          rows={Math.min(14, Math.max(3, expr.split("\n").length + 1))}
          value={expr}
          onChange={(e) => setExpr(e.target.value)}
        />
      ) : (
        <FormWidget widget={widget} value={value} onChange={setValue} />
      )}

      {error && <p className="error">{error}</p>}
      <div className="row">
        <button className="primary" disabled={busy} onClick={stage}>
          {fileExpr !== null ? "Stage change" : "Add to file"}
        </button>
        {fileExpr !== null && (
          <button className="danger" disabled={busy} onClick={() => act(() => api.stageRemoveOption(loc))}>
            Remove from file
          </button>
        )}
      </div>
    </div>
  );
}

function FormWidget({
  widget,
  value,
  onChange,
}: {
  widget: Widget;
  value: FormValue;
  onChange: (v: FormValue) => void;
}) {
  const nullable = "nullable" in widget && widget.nullable;
  const nullToggle = nullable && (
    <label className="checkbox">
      <input
        type="checkbox"
        checked={value === null}
        onChange={(e) => onChange(e.target.checked ? null : emptyValue({ ...widget, nullable: false } as Widget))}
      />
      null
    </label>
  );

  switch (widget.kind) {
    case "bool":
      return (
        <div className="row">
          <label className="switch">
            <input type="checkbox" checked={value === true} disabled={value === null} onChange={(e) => onChange(e.target.checked)} />
            <span>{value === null ? "null" : value ? "true" : "false"}</span>
          </label>
          {nullToggle}
        </div>
      );
    case "string":
      return (
        <div className="stack">
          {value !== null &&
            (widget.multiline ? (
              <textarea className="code-input" rows={8} value={value as string} onChange={(e) => onChange(e.target.value)} />
            ) : (
              <input type="text" value={value as string} onChange={(e) => onChange(e.target.value)} />
            ))}
          {nullToggle}
        </div>
      );
    case "number":
      return (
        <div className="row">
          {value !== null && (
            <input
              type="number"
              step={widget.integer ? 1 : "any"}
              value={value as number}
              onChange={(e) => onChange(widget.integer ? parseInt(e.target.value || "0", 10) : Number(e.target.value))}
            />
          )}
          {nullToggle}
        </div>
      );
    case "enum":
      return (
        <div className="row">
          {value !== null && (
            <select value={value as string} onChange={(e) => onChange(e.target.value)}>
              {widget.values.map((v) => (
                <option key={v}>{v}</option>
              ))}
            </select>
          )}
          {nullToggle}
        </div>
      );
    case "stringList": {
      const items = value as string[];
      return (
        <div className="stack">
          {items.map((item, i) => (
            <div className="row" key={i}>
              <input type="text" value={item} onChange={(e) => onChange(items.map((x, j) => (j === i ? e.target.value : x)))} />
              <button className="icon" title="Remove" onClick={() => onChange(items.filter((_, j) => j !== i))}>
                ×
              </button>
            </div>
          ))}
          <button className="link" onClick={() => onChange([...items, ""])}>
            + Add item
          </button>
        </div>
      );
    }
    case "stringMap": {
      const entries = value as [string, string][];
      const update = (i: number, k: string, v: string) => onChange(entries.map((e, j): [string, string] => (j === i ? [k, v] : e)));
      return (
        <div className="stack">
          {entries.map(([k, v], i) => (
            <div className="row" key={i}>
              <input className="key" type="text" placeholder="name" value={k} onChange={(e) => update(i, e.target.value, v)} />
              <span className="muted">=</span>
              <input type="text" placeholder="value" value={v} onChange={(e) => update(i, k, e.target.value)} />
              <button className="icon" title="Remove" onClick={() => onChange(entries.filter((_, j) => j !== i))}>
                ×
              </button>
            </div>
          ))}
          <button className="link" onClick={() => onChange([...entries, ["", ""]])}>
            + Add entry
          </button>
        </div>
      );
    }
    case "raw":
      return null;
  }
}

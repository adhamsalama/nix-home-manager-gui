import { useEffect, useState } from "react";
import { api, errorText, type Settings, type Status } from "../api";

export function SettingsPage({ status, onChanged }: { status: Status | null; onChanged: () => void }) {
  const [settings, setSettings] = useState<Settings | null>(null);
  const [message, setMessage] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    api.getSettings().then(setSettings);
  }, []);

  if (!settings) return <div className="page centered"><div className="spinner" /></div>;

  const set = (patch: Partial<Settings>) => setSettings({ ...settings, ...patch });

  const setMode = async (mode: Settings["mode"]) => {
    set({ mode, configFile: await api.defaultConfigFile(mode) });
  };

  const save = async () => {
    try {
      await api.saveSettings(settings);
      setMessage({ ok: true, text: "Settings saved." });
      onChanged();
    } catch (e) {
      setMessage({ ok: false, text: errorText(e) });
    }
  };

  return (
    <div className="page narrow">
      <h2>Settings</h2>

      <fieldset>
        <legend>Backend</legend>
        <label className="radio">
          <input type="radio" checked={settings.mode === "real"} onChange={() => setMode("real")} />
          <span>
            <b>Real</b>: use the installed nix and home-manager
            {status && !status.nixAvailable && <span className="error"> (nix wasn't found)</span>}
          </span>
        </label>
        <label className="radio">
          <input type="radio" checked={settings.mode === "mock"} onChange={() => setMode("mock")} />
          <span>
            <b>Mock</b>: sample data and simulated commands. Edits still go to a real sample file.
          </span>
        </label>
      </fieldset>

      <fieldset>
        <legend>Configuration</legend>
        <label>
          <span>Nix file to edit</span>
          <input type="text" value={settings.configFile} onChange={(e) => set({ configFile: e.target.value })} />
          <small className="muted">Usually your home.nix. Options set in other imported files are read-only here.</small>
        </label>
        <label>
          <span>Flake output (optional)</span>
          <input
            type="text"
            placeholder="~/.config/home-manager#yourname"
            value={settings.flakeRef}
            onChange={(e) => set({ flakeRef: e.target.value })}
          />
          <small className="muted">
            Leave empty for a classic setup (<code>home-manager -f file</code>). For flakes, this is passed to{" "}
            <code>--flake</code> and used to evaluate <code>homeConfigurations.&lt;name&gt;</code>.
          </small>
        </label>
      </fieldset>

      <fieldset disabled={settings.mode === "mock"}>
        <legend>Option documentation</legend>
        <p className="muted small">
          Options are read from the home-manager you have installed: your <code>home-manager</code> channel in a
          classic setup, or the <code>home-manager</code> input of your flake. The flake below is only a fallback.
        </p>
        <label>
          <span>Fallback home-manager flake</span>
          <input type="text" value={settings.homeManagerFlake} onChange={(e) => set({ homeManagerFlake: e.target.value })} />
          <small className="muted">
            Pin it to your release, e.g. <code>github:nix-community/home-manager/release-25.05</code>.
          </small>
        </label>
        <label>
          <span>options.json path (optional)</span>
          <input type="text" value={settings.optionsJson} onChange={(e) => set({ optionsJson: e.target.value })} />
          <small className="muted">Overrides everything above.</small>
        </label>
      </fieldset>

      <div className="row">
        <button className="primary" onClick={save}>
          Save settings
        </button>
        {message && <span className={message.ok ? "muted" : "error"}>{message.text}</span>}
      </div>
    </div>
  );
}

import { useEffect, useState } from "react";
import { api, errorText, type Channel, type Channels } from "../api";
import { RunLog, useRun } from "../components/RunLog";

const NIXPKGS_URL = "https://nixos.org/channels/";
const HM_URL = "https://github.com/nix-community/home-manager/archive/";

/** The two newest stable releases (they come out in May and November). */
function stableReleases(now = new Date()): string[] {
  const yy = now.getFullYear() % 100;
  const month = now.getMonth() + 1;
  const latest = month >= 11 ? [yy, 11] : month >= 5 ? [yy, 5] : [yy - 1, 11];
  const previous = latest[1] === 11 ? [latest[0], 5] : [latest[0] - 1, 11];
  return [latest, previous].map(([y, m]) => `${y}.${String(m).padStart(2, "0")}`);
}

function suggestedUrls(): string[] {
  const mac = navigator.userAgent.includes("Mac");
  const releases = stableReleases();
  return [
    `${NIXPKGS_URL}nixpkgs-unstable`,
    ...(mac ? [] : [`${NIXPKGS_URL}nixos-unstable`]),
    ...releases.map((r) => (mac ? `${NIXPKGS_URL}nixpkgs-${r}-darwin` : `${NIXPKGS_URL}nixos-${r}`)),
    `${HM_URL}master.tar.gz`,
    ...releases.map((r) => `${HM_URL}release-${r}.tar.gz`),
  ];
}

const formatTime = (secs: number | null) => (secs ? new Date(secs * 1000).toLocaleString() : "never");

export function ChannelsPage({ onNavigate }: { onNavigate: (page: "apply") => void }) {
  const [channels, setChannels] = useState<Channels | null>(null);
  const [usesFlake, setUsesFlake] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [editing, setEditing] = useState<{ name: string; url: string } | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const [newChannel, setNewChannel] = useState({ name: "", url: "" });
  const [updateTarget, setUpdateTarget] = useState<string | null>(null);

  const load = () =>
    api
      .listChannels()
      .then((c) => {
        setChannels(c);
        setError(null);
      })
      .catch((e) => setError(errorText(e)));

  const run = useRun(() => load());

  useEffect(() => {
    load();
    api.getSettings().then((s) => setUsesFlake(s.flakeRef.trim() !== ""));
  }, []);

  const act = async (f: () => Promise<unknown>, done?: string) => {
    setError(null);
    setMessage(null);
    try {
      await f();
      if (done) setMessage(done);
      await load();
    } catch (e) {
      setError(errorText(e));
    }
  };

  const update = async (name: string | null) => {
    setConfirming(null);
    setMessage(null);
    setUpdateTarget(name);
    try {
      await run.start(() => api.startChannelUpdate(name));
    } catch (e) {
      setError(errorText(e));
    }
  };

  const saveUrl = (name: string, url: string) =>
    act(async () => {
      await api.addChannel(name, url);
      setEditing(null);
    }, `${name} now points at the new URL. Update it to download the new version.`);

  return (
    <div className="page">
      <h2>Channels</h2>
      <p className="muted">
        In a classic setup, <code>&lt;nixpkgs&gt;</code> and <code>&lt;home-manager&gt;</code> come from your channels.
        Updating a channel moves every package to that channel's latest versions; nothing changes until you build and
        switch.
      </p>

      {usesFlake && (
        <p className="notice">
          Your settings use a flake, so channels don't affect your builds. Flake inputs are pinned in{" "}
          <code>flake.lock</code> instead.
        </p>
      )}
      {channels?.warnings.map((w) => (
        <p key={w} className="notice warn-notice">
          {w}
        </p>
      ))}
      {error && <pre className="error-block">{error}</pre>}
      {message && <p className="notice">{message}</p>}
      {!channels && !error && <div className="spinner" />}

      {channels && (
        <>
          <div className="section-head">
            <h3>Your channels</h3>
            <span className="muted small">Last updated {formatTime(channels.userUpdated)}</span>
          </div>
          <table className="results">
            <thead>
              <tr>
                <th>Name</th>
                <th>Source</th>
                <th>Version</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {channels.user.length === 0 && (
                <tr>
                  <td colSpan={4} className="muted">
                    No channels yet. Add nixpkgs and home-manager below.
                  </td>
                </tr>
              )}
              {channels.user.map((c) =>
                editing?.name === c.name ? (
                  <tr key={c.name}>
                    <td className="option-name">{c.name}</td>
                    <td colSpan={3}>
                      <form
                        className="row"
                        onSubmit={(e) => {
                          e.preventDefault();
                          saveUrl(c.name, editing.url);
                        }}
                      >
                        <input
                          type="text"
                          list="channel-urls"
                          value={editing.url}
                          autoFocus
                          onChange={(e) => setEditing({ ...editing, url: e.target.value })}
                        />
                        <button className="primary">Save</button>
                        <button type="button" onClick={() => setEditing(null)}>
                          Cancel
                        </button>
                      </form>
                    </td>
                  </tr>
                ) : (
                  <ChannelRow
                    key={c.name}
                    channel={c}
                    busy={run.running}
                    confirming={confirming === c.name}
                    onUpdate={() => update(c.name)}
                    onEdit={() => setEditing({ name: c.name, url: c.url ?? "" })}
                    onAskRemove={() => setConfirming(c.name)}
                    onCancel={() => setConfirming(null)}
                    onRemove={() => {
                      setConfirming(null);
                      act(() => api.removeChannel(c.name), `Removed ${c.name}.`);
                    }}
                  />
                ),
              )}
            </tbody>
          </table>

          <div className="row actions">
            <button className="primary" disabled={run.running || channels.user.length === 0} onClick={() => update(null)}>
              Update all channels
            </button>
            {confirming === "@rollback" ? (
              <>
                <button
                  className="danger"
                  onClick={() => {
                    setConfirming(null);
                    act(api.rollbackChannels, "Restored the previous channels.");
                  }}
                >
                  Restore previous channels
                </button>
                <button onClick={() => setConfirming(null)}>Cancel</button>
              </>
            ) : (
              <button disabled={run.running} onClick={() => setConfirming("@rollback")}>
                Undo last update…
              </button>
            )}
          </div>

          <h3>Add a channel</h3>
          <form
            className="row"
            onSubmit={(e) => {
              e.preventDefault();
              act(async () => {
                await api.addChannel(newChannel.name, newChannel.url);
                setNewChannel({ name: "", url: "" });
              }, `Added ${newChannel.name}. Update it to download it.`);
            }}
          >
            <input
              className="key"
              type="text"
              placeholder="name, e.g. nixpkgs"
              value={newChannel.name}
              onChange={(e) => setNewChannel({ ...newChannel, name: e.target.value })}
            />
            <input
              type="text"
              list="channel-urls"
              placeholder="https://nixos.org/channels/nixpkgs-unstable"
              value={newChannel.url}
              onChange={(e) => setNewChannel({ ...newChannel, url: e.target.value })}
            />
            <button disabled={!newChannel.name.trim() || !newChannel.url.trim()}>Add</button>
          </form>
          <p className="muted small">
            Adding a channel with an existing name replaces its URL. Use the same release for nixpkgs and home-manager,
            e.g. <code>nixpkgs-unstable</code> with <code>master</code>.
          </p>

          <datalist id="channel-urls">
            {suggestedUrls().map((u) => (
              <option key={u} value={u} />
            ))}
          </datalist>

          <RunLog run={run} title={`nix-channel --update ${updateTarget ?? ""}`} />
          {run.done?.success && (
            <p className="notice">
              Channels updated. Build to see whether your configuration still works, then switch to apply the new
              versions.{" "}
              <button className="link" onClick={() => onNavigate("apply")}>
                Go to Review &amp; apply
              </button>
            </p>
          )}

          {channels.system.length > 0 && (
            <>
              <div className="section-head">
                <h3>System channels (root)</h3>
                <span className="muted small">Last updated {formatTime(channels.systemUpdated)}</span>
              </div>
              <p className="muted">
                These belong to root and are used when you don't have a channel with the same name. To update them, run{" "}
                <code>sudo nix-channel --update</code> in a terminal.
              </p>
              <table className="results">
                <tbody>
                  {channels.system.map((c) => (
                    <tr key={c.name}>
                      <td className="option-name">{c.name}</td>
                      <td>
                        <Version channel={c} />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </>
          )}
        </>
      )}
    </div>
  );
}

function Version({ channel }: { channel: Channel }) {
  if (!channel.version) return <span className="muted">Not downloaded yet</span>;
  return (
    <>
      <div className="option-name">{channel.version}</div>
      {channel.revision && <div className="muted small">rev {channel.revision}</div>}
    </>
  );
}

function ChannelRow(props: {
  channel: Channel;
  busy: boolean;
  confirming: boolean;
  onUpdate: () => void;
  onEdit: () => void;
  onAskRemove: () => void;
  onCancel: () => void;
  onRemove: () => void;
}) {
  const { channel: c } = props;
  return (
    <tr>
      <td className="option-name">{c.name}</td>
      <td className="muted path" title={c.url ?? ""}>
        {c.url ?? "—"}
      </td>
      <td>
        <Version channel={c} />
      </td>
      <td className="nowrap">
        {props.confirming ? (
          <span className="row">
            <button className="danger" onClick={props.onRemove}>
              Remove {c.name}
            </button>
            <button onClick={props.onCancel}>Cancel</button>
          </span>
        ) : (
          <span className="row">
            <button disabled={props.busy} onClick={props.onUpdate}>
              Update
            </button>
            <button onClick={props.onEdit}>Change URL</button>
            <button className="icon" title={`Remove ${c.name}`} onClick={props.onAskRemove}>
              ×
            </button>
          </span>
        )}
      </td>
    </tr>
  );
}

import { useCallback, useEffect, useState } from "react";
import { api, type Status } from "./api";
import { ApplyPage } from "./pages/ApplyPage";
import { ChannelsPage } from "./pages/ChannelsPage";
import { GenerationsPage } from "./pages/GenerationsPage";
import { OptionsPage, type OptionFocus } from "./pages/OptionsPage";
import { PackagesPage } from "./pages/PackagesPage";
import { SettingsPage } from "./pages/SettingsPage";
import "./App.css";

type Page = "options" | "packages" | "apply" | "generations" | "channels" | "settings";

const NAV: { id: Page; label: string }[] = [
  { id: "options", label: "Options" },
  { id: "packages", label: "Packages" },
  { id: "apply", label: "Review & apply" },
  { id: "generations", label: "Generations" },
  { id: "channels", label: "Channels" },
  { id: "settings", label: "Settings" },
];

export default function App() {
  const [page, setPage] = useState<Page>("options");
  const [status, setStatus] = useState<Status | null>(null);
  // Bumped after every edit so pages re-read the working copy.
  const [revision, setRevision] = useState(0);
  // Bumped when settings change, which remounts pages with fresh data.
  const [settingsVersion, setSettingsVersion] = useState(0);
  const [optionFocus, setOptionFocus] = useState<OptionFocus | null>(null);

  const openOption = (name: string) => {
    setOptionFocus({ name, nonce: Date.now() });
    setPage("options");
  };

  const refresh = useCallback(() => {
    setRevision((r) => r + 1);
    api.getStatus().then(setStatus);
  }, []);

  useEffect(() => {
    refresh();
    // The file may have been edited elsewhere while the window was in the background.
    window.addEventListener("focus", refresh);
    return () => window.removeEventListener("focus", refresh);
  }, [refresh]);

  const onSettingsChanged = () => {
    setSettingsVersion((v) => v + 1);
    refresh();
  };

  const props = { revision, onChanged: refresh };

  return (
    <div className="app">
      <nav className="sidebar">
        <div className="brand">Home Manager</div>
        {NAV.map((n) => (
          <button key={n.id} className={page === n.id ? "nav active" : "nav"} onClick={() => setPage(n.id)}>
            {n.label}
            {n.id === "apply" && status?.dirty && <span className="pill">unsaved</span>}
          </button>
        ))}
        <div className="sidebar-footer">
          {status && (
            <>
              <span className={status.mode === "mock" ? "badge warn" : "badge ok"}>
                {status.mode === "mock" ? "Mock mode" : "Real mode"}
              </span>
              <div className="file-path" title={status.configFile}>
                {status.configFile}
              </div>
            </>
          )}
        </div>
      </nav>

      <main key={settingsVersion}>
        {status?.fileError && (
          <div className="banner error-banner">
            {status.fileError}
            <button className="link" onClick={() => setPage("settings")}>
              Open settings
            </button>
          </div>
        )}
        {status?.mode === "real" && !status.homeManagerAvailable && (
          <div className="banner">home-manager wasn't found on PATH. Build, switch and generations won't work.</div>
        )}
        {/* Pages stay mounted so searches and scroll positions survive tab switches. */}
        <div className="page-host" hidden={page !== "options"}>
          <OptionsPage {...props} focus={optionFocus} />
        </div>
        <div className="page-host" hidden={page !== "packages"}>
          <PackagesPage {...props} onOpenOption={openOption} />
        </div>
        <div className="page-host" hidden={page !== "apply"}>
          <ApplyPage {...props} />
        </div>
        <div className="page-host" hidden={page !== "generations"}>
          <GenerationsPage revision={revision} />
        </div>
        <div className="page-host" hidden={page !== "channels"}>
          <ChannelsPage onNavigate={setPage} />
        </div>
        <div className="page-host" hidden={page !== "settings"}>
          <SettingsPage status={status} onChanged={onSettingsChanged} />
        </div>
      </main>
    </div>
  );
}

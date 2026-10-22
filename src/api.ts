import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

export type Mode = "mock" | "real";

export interface Status {
  mode: Mode;
  configFile: string;
  nixAvailable: boolean;
  homeManagerAvailable: boolean;
  dirty: boolean;
  fileError: string | null;
}

export interface Settings {
  mode: Mode;
  configFile: string;
  flakeRef: string;
  optionsJson: string;
  homeManagerFlake: string;
}

export interface HmOption {
  name: string;
  loc: string[];
  type: string;
  description: string;
  default: string | null;
  example: string | null;
  declarations: string[];
  readOnly: boolean;
}

export type FileValue =
  | { state: "notSet" }
  | { state: "set"; expr: string; line: number }
  | { state: "partial"; line: number }
  | { state: "opaque"; line: number; expr: string };

export interface FileOverview {
  path: string;
  dirty: boolean;
  definedPaths: string[];
}

export interface Packages {
  defined: boolean;
  withPkgs: boolean;
  items: string[];
}

export interface PackageInfo {
  attr: string;
  pname: string;
  version: string;
  description: string;
}

export interface PackageDetails {
  attr: string;
  name: string | null;
  version: string | null;
  description: string | null;
  homepage: string | null;
  licenses: string[];
  source: string;
}

export interface SearchResults {
  source: string;
  packages: PackageInfo[];
}

export interface DiffLine {
  kind: "hunk" | "context" | "add" | "del";
  text: string;
  oldLine: number | null;
  newLine: number | null;
}

export interface Changes {
  path: string;
  dirty: boolean;
  diff: DiffLine[];
}

export interface Generation {
  id: number;
  date: string;
  path: string;
  current: boolean;
}

export interface Channel {
  name: string;
  url: string | null;
  version: string | null;
  revision: string | null;
  kind: "nixpkgs" | "home-manager" | "other";
}

export interface Channels {
  user: Channel[];
  system: Channel[];
  userUpdated: number | null;
  systemUpdated: number | null;
  warnings: string[];
}

export type StagedValue = { kind: "json"; value: unknown } | { kind: "raw"; expr: string };

export interface LogLine {
  runId: string;
  stream: "stdout" | "stderr" | "info";
  line: string;
}

export interface RunDone {
  runId: string;
  success: boolean;
  code: number | null;
}

export const api = {
  getStatus: () => invoke<Status>("get_status"),
  getSettings: () => invoke<Settings>("get_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  defaultConfigFile: (mode: Mode) => invoke<string>("default_config_file", { mode }),
  loadOptions: (refresh = false) => invoke<HmOption[]>("load_options", { refresh }),
  getFileOverview: () => invoke<FileOverview>("get_file_overview"),
  getOptionInFile: (loc: string[]) => invoke<FileValue>("get_option_in_file", { loc }),
  stageOption: (loc: string[], value: StagedValue) => invoke<void>("stage_option", { loc, value }),
  stageRemoveOption: (loc: string[]) => invoke<void>("stage_remove_option", { loc }),
  getPackages: () => invoke<Packages>("get_packages"),
  stageAddPackage: (name: string) => invoke<void>("stage_add_package", { name }),
  stageRemovePackage: (name: string) => invoke<void>("stage_remove_package", { name }),
  searchPackages: (query: string) => invoke<SearchResults>("search_packages", { query }),
  packageDetails: (attr: string) => invoke<PackageDetails>("package_details", { attr }),
  getChanges: () => invoke<Changes>("get_changes"),
  saveChanges: () => invoke<void>("save_changes"),
  discardChanges: () => invoke<void>("discard_changes"),
  evalOption: (loc: string[], defaultValue: string | null) =>
    invoke<string>("eval_option", { loc, default: defaultValue }),
  startRun: (action: "build" | "switch") => invoke<string>("start_run", { action }),
  listGenerations: () => invoke<Generation[]>("list_generations"),
  activateGeneration: (id: number) => invoke<string>("activate_generation", { id }),
  listChannels: () => invoke<Channels>("list_channels"),
  addChannel: (name: string, url: string) => invoke<void>("add_channel", { name, url }),
  removeChannel: (name: string) => invoke<void>("remove_channel", { name }),
  startChannelUpdate: (name: string | null) => invoke<string>("start_channel_update", { name }),
  rollbackChannels: () => invoke<string>("rollback_channels"),
};

export function onRunLog(cb: (l: LogLine) => void): Promise<UnlistenFn> {
  return listen<LogLine>("run-log", (e) => cb(e.payload));
}

export function onRunDone(cb: (d: RunDone) => void): Promise<UnlistenFn> {
  return listen<RunDone>("run-done", (e) => cb(e.payload));
}

/** Tauri rejects with the command's error string. */
export function errorText(e: unknown): string {
  return typeof e === "string" ? e : e instanceof Error ? e.message : JSON.stringify(e);
}

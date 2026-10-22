import type { PackageDetails } from "./api";

const OS_LABELS: Record<string, string> = {
  linux: "Linux",
  darwin: "macOS",
  freebsd: "FreeBSD",
  openbsd: "OpenBSD",
  netbsd: "NetBSD",
  windows: "Windows",
  cygwin: "Cygwin",
};

export const osLabel = (os: string) => OS_LABELS[os] ?? os;

/** Groups systems like `x86_64-linux` by OS: `[["Linux", ["x86_64", "aarch64"]], ...]`. */
export function groupPlatforms(platforms: string[]): [string, string[]][] {
  const groups = new Map<string, string[]>();
  for (const p of platforms) {
    const dash = p.indexOf("-");
    const [arch, os] = dash === -1 ? ["", p] : [p.slice(0, dash), p.slice(dash + 1)];
    const label = osLabel(os);
    const arches = groups.get(label) ?? [];
    // `*-linux` is a pattern meaning every architecture.
    const name = arch === "*" ? "all architectures" : arch;
    if (name && !arches.includes(name)) arches.push(name);
    groups.set(label, arches);
  }
  return [...groups.entries()];
}

/** Why a package won't build on this machine, or null if it will. */
export function unavailableReason(d: PackageDetails): string | null {
  if (d.available) return null;
  const system = d.system ?? "this system";
  if (d.broken) return `${d.attr} is marked broken in nixpkgs, so it won't build.`;
  if (d.platforms.length > 0 && d.system && !d.platforms.some((p) => matches(p, d.system!))) {
    const supported = groupPlatforms(d.platforms)
      .map(([os]) => os)
      .join(", ");
    return `${d.attr} isn't available on ${system}. It supports ${supported}.`;
  }
  if (d.badPlatforms.some((p) => d.system && matches(p, d.system))) {
    return `${d.attr} is marked as not working on ${system}.`;
  }
  if (d.unfree) {
    return `${d.attr} is unfree. It only builds with nixpkgs.config.allowUnfree = true.`;
  }
  return `${d.attr} can't be built on ${system} with your current nixpkgs settings (it may be marked insecure).`;
}

function matches(platform: string, system: string): boolean {
  return platform === system || (platform.startsWith("*-") && system.endsWith(platform.slice(1)));
}

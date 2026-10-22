import type { HmOption } from "./api";

export interface RelatedOptions {
  /** Module prefixes such as `programs.git`. */
  modules: string[];
  options: HmOption[];
}

const escapeRegExp = (s: string) => s.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");

/**
 * Finds the home-manager modules that configure a package: modules whose
 * `package` option defaults to `pkgs.<attr>`, plus `programs.<name>` and
 * `services.<name>` by name, since not every module has a `package` option.
 */
export function relatedOptions(options: HmOption[], attr: string): RelatedOptions {
  const byDefault = new RegExp(`^pkgs\\.${escapeRegExp(attr)}$`);
  const modules = new Set<string>();
  for (const o of options) {
    if (o.loc[o.loc.length - 1] === "package" && o.default && byDefault.test(o.default.trim())) {
      modules.add(o.loc.slice(0, -1).join("."));
    }
  }
  const name = attr.split(".").pop()!;
  for (const prefix of [`programs.${name}`, `services.${name}`]) {
    if (options.some((o) => o.name.startsWith(`${prefix}.`))) modules.add(prefix);
  }

  const prefixes = [...modules].map((m) => `${m}.`);
  // Each module's own `enable` first: it's the option people want.
  const enables = new Set([...modules].map((m) => `${m}.enable`));
  const related = options
    .filter((o) => prefixes.some((p) => o.name.startsWith(p)))
    .sort((a, b) => Number(enables.has(b.name)) - Number(enables.has(a.name)) || a.name.localeCompare(b.name));
  return { modules: [...modules].sort(), options: related };
}

# Home Manager GUI

A desktop GUI for [home-manager](https://github.com/nix-community/home-manager), built with Tauri 2 (Rust) and React.

- **Options**: search every home-manager option and set it in your `home.nix`. Edits are made in place with [rnix](https://github.com/nix-community/rnix-parser), so comments and formatting are preserved.
- **Packages**: search nixpkgs and add or remove entries in `home.packages`.
- **Review & apply**: see a diff of staged edits, save (a `.nix.bak` backup is kept), then run `home-manager build` or `switch` with live output.
- **Generations**: list generations and roll back to one.
- **Channels** (classic setups): list, add, change, remove and update your nix channels, undo the last update, and see root's channels read-only. Warns when nixpkgs and home-manager are on different releases.

## Development

Requires Rust, Node and pnpm. On Linux, also install Tauri's [system dependencies](https://v2.tauri.app/start/prerequisites/#linux).

```bash
pnpm install
pnpm tauri dev
```

Without nix installed, the app starts in **mock mode**: fixture options, packages and generations, and simulated build output. File edits still go to a real sample file. Switch to real mode in Settings.

```bash
cd src-tauri && cargo test   # editing, parsing and diff tests
```

## Packages

`.github/workflows/build.yml` builds `.deb` and `.rpm` (Linux) and a universal `.dmg` (macOS). Pushing a `v*` tag creates a draft release with them.

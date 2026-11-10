# Home Manager GUI

A desktop GUI for [home-manager](https://github.com/nix-community/home-manager), written in Rust with a [Slint](https://slint.dev) UI. It idles at roughly 30–60 MB.

- **Options**: search every home-manager option and set it in your `home.nix`. Edits are made in place with [rnix](https://github.com/nix-community/rnix-parser), so comments and formatting are preserved.
- **Packages**: search nixpkgs and add or remove entries in `home.packages`.
- **Review & apply**: see a diff of staged edits, save (a `.nix.bak` backup is kept), then run `home-manager build` or `switch` with live output.
- **Generations**: list generations and roll back to one.
- **Channels** (classic setups): list, add, change, remove and update your nix channels, undo the last update, and see root's channels read-only. Warns when nixpkgs and home-manager are on different releases.

## Development

Requires Rust. On Linux, also install fontconfig and xkbcommon headers (on Debian/Ubuntu: `libfontconfig1-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libx11-xcb-dev`; on Fedora: `fontconfig-devel libxkbcommon-devel libxkbcommon-x11-devel wayland-devel libX11-devel`).

```bash
cargo run
```

The UI lives in `ui/*.slint`; `src/app.rs` connects it to the backend. For layout work, `cargo run --features snapshot` with `HM_GUI_SNAPSHOT=out.png` saves a screenshot and quits; `HM_GUI_PAGE=<0-5>`, `HM_GUI_SELECT=<option>`, `HM_GUI_PACKAGE=<attr>` and `HM_GUI_QUERY=<package search>` pick what's shown.

Without nix installed, the app starts in **mock mode**: fixture options, packages and generations, and simulated build output. File edits still go to a real sample file. Switch to real mode in Settings.

```bash
cargo test   # editing, parsing, form and diff tests
```

## Packages

`.github/workflows/build.yml` builds `.deb` and `.rpm` (Linux) and a universal `.dmg` (macOS) with [cargo-bundle](https://github.com/burtonageo/cargo-bundle), and the `.rpm` with [cargo-generate-rpm](https://github.com/cat-in-136/cargo-generate-rpm). Pushing a `v*` tag creates a draft release with them.

## License

MIT, see [LICENSE](LICENSE). The UI is built with [Slint](https://slint.dev), used under its royalty-free license.

[![Made with Slint](https://raw.githubusercontent.com/slint-ui/slint/master/logo/MadeWithSlint-logo-whitebg.png)](https://slint.dev)

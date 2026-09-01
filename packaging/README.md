# Packaging

App-id: `io.github.f3nr1l.WinctrlConfig` (placeholder — finalize to the real
project slug before publishing). Shared assets in this directory:

- `io.github.f3nr1l.WinctrlConfig.desktop` — desktop entry (`@BINARY@` is
  replaced with the binary path/name at install time).
- `io.github.f3nr1l.WinctrlConfig.metainfo.xml` — AppStream metadata (fill in the
  real URLs and screenshots before submitting to a store).
- `icons/hicolor/**` — application icons.

## Local install (no packaging)

`./install.sh` installs the release binary, icons, desktop entry and translation
catalogs into `~/.local` for the current user (no `sudo`).

## Flatpak

See `flatpak/`. Build and install locally:

```sh
cd flatpak
flatpak-builder --user --install --force-clean build io.github.f3nr1l.WinctrlConfig.yml
flatpak run io.github.f3nr1l.WinctrlConfig
```

Notes:
- Adjust `runtime-version` and the `org.freedesktop.Sdk.Extension.rust-stable`
  branch to what is installed (`flatpak install org.gnome.Sdk//48
  org.freedesktop.Sdk.Extension.rust-stable//24.08`).
- The manifest builds with network access (cargo fetches crates). For a fully
  offline / Flathub-style build, drop `--share=network` from `build-args` and add a
  generated cargo sources file:
  ```sh
  python3 flatpak-cargo-generator.py ../../rust/Cargo.lock -o cargo-sources.json
  ```
  then reference `cargo-sources.json` under the module `sources:` and add
  `--offline` to the cargo command. (`flatpak-cargo-generator.py` comes from the
  flatpak-builder-tools repository.)

## AppImage

See `appimage/build-appimage.sh`. It downloads `linuxdeploy` and
`linuxdeploy-plugin-gtk`, builds the release binaries, assembles an AppDir
(binaries, desktop entry, icons, metadata, translations) and bundles the GTK 4 /
libadwaita stack into a single `.AppImage`. Requires a Rust toolchain, the
gtk4/libadwaita development files, `msgfmt`, `curl` and FUSE.

```sh
./appimage/build-appimage.sh
```

## Build notes (0.9.0)

Both packages build and install successfully:

- **AppImage** — `packaging/appimage/build-appimage.sh` (set `APPIMAGE_EXTRACT_AND_RUN=1`
  on hosts without FUSE). Produces `WinCtrl_Config-x86_64.AppImage`.
- **Flatpak** — `flatpak-builder` against the manifest.

Two things to revisit before a Flathub submission:

1. **Rust toolchain.** gtk4-rs 0.11 requires a newer `rustc` than the stable SDK
   extension currently ships (`rust-stable//24.08` = 1.89 vs. the crates' 1.92). The
   local 0.9.0 Flatpak was built by swapping `rust-stable` → `rust-nightly` in the
   manifest. Keep `rust-stable` in the committed manifest and drop the nightly swap
   once the stable extension catches up (or pin the gtk-rs stack to a 1.89-MSRV set).
2. **Runtime version.** The manifest targets `org.gnome.Platform//48`, which is
   end-of-life. Bump to a supported GNOME runtime before publishing.

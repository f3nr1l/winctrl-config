# Translations

WinCtrl Config is internationalized with **gettext**. The source strings are in
**English**; translations live here as `<lang>.po` files (gettext domain
`winctrl`).

- `winctrl.pot` — the translation template (all translatable strings).
- `fr.po` — French translation.
- `LINGUAS` — the list of shipped languages, one code per line.

## Adding a language

1. Create the catalog from the template (replace `de` with your language code):

   ```sh
   msginit --no-translator -l de -i winctrl.pot -o de.po
   ```

2. Translate every `msgstr` in `de.po`.

3. Add the language code on its own line in `LINGUAS`.

4. Check and preview:

   ```sh
   msgfmt --check --check-format -o /dev/null de.po
   ```

## Updating after code changes

Re-extract the template and merge it into the existing catalogs:

```sh
xtr -k tr -k tr_f -k "trn:1,2" -k "trn_f:1,2" -o winctrl.pot ../src/lib.rs
xtr -k tr -k tr_f -k "trn:1,2" -k "trn_f:1,2" -o /tmp/bin.pot ../src/bin/wwctrl.rs
msgcat --use-first winctrl.pot /tmp/bin.pot -o winctrl.pot
msgmerge --update fr.po winctrl.pot
```

(`xtr` is a Rust-aware gettext extractor: `cargo install xtr`.)

## Installation

`packaging/install.sh` compiles each `<lang>.po` to
`~/.local/share/locale/<lang>/LC_MESSAGES/winctrl.mo`. The application also honors
the `WINCTRL_LOCALEDIR` environment variable to point at a custom locale
directory.

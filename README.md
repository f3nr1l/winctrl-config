# WinCtrl Config

Application de configuration pour les manches **WinWing URSA MINOR** sous Linux
(GTK4 / libadwaita), écrite en Rust.

> Projet **tiers et non officiel** : ni affilié à WinWing, ni endossé par lui.
> « WINWING », « WINCTRL » et « URSA MINOR » sont des marques de leurs détenteurs
> respectifs. Fourni « en l'état », sans aucune garantie ; les écritures dans la
> mémoire du matériel se font aux risques de l'utilisateur.

## Fonctionnalités

- **Général** : identité du manche, mode de l'axe de lacet, zones mortes, mode
  de répartition des boutons, vérification de la version de firmware, réinitialisation
  d'usine.
- **Rétroéclairage** : luminosité (aperçu en direct et enregistrement), mode fixe /
  respiration.
- **Calibration** : assistant de calibration des axes.
- **Vibration** : test du moteur et éditeur de courbes d'effet.
- **Profils** : capture, import et export de la configuration.
- **Boutons** : moniteur d'entrées en direct (axes et boutons).
- **Remap** : réaffectation des boutons et répartition en manettes virtuelles
  (contournement de la limite de 32 boutons de certains jeux), côté système, sans
  écriture dans le manche.

Toute écriture dans la mémoire du manche passe par une confirmation explicite, avec
une sauvegarde automatique préalable, une écriture *diff-only* et une relecture de
vérification. Les registres d'identité (nom, PID, numéro de série) sont protégés en
dur et ne sont jamais écrits.

## Construire

Prérequis : la chaîne d'outils Rust (via `rustup`), GTK 4 et libadwaita (paquets de
développement).

```sh
cargo build --release            # binaire GUI + CLI
cargo test                       # tests unitaires
cargo clippy --all-targets -- -D warnings
```

Binaires produits :

- `winctrl-gui` — l'application graphique ;
- `wwctrl` — un utilitaire en ligne de commande (lecture seule) pour lister les
  manches et afficher la configuration décodée.

Le cœur (protocole, énumération, modèle) se compile sans GTK avec
`cargo build --no-default-features`.

## Internationalisation

L'interface et la CLI sont internationalisées avec **gettext**. Les chaînes source
sont en **anglais** ; les traductions vivent dans `po/` (modèle `winctrl.pot`,
français `fr.po`, domaine gettext `winctrl`). Voir `po/README.md` pour ajouter une
langue. `packaging/install.sh` compile et installe les catalogues.

## Accès au matériel

Les manches sont exposés via `hidraw` et `/dev/input`. Installez la règle d'accès
`udev` fournie (répertoire `udev/`) pour utiliser l'application sans droits
supplémentaires. La réaffectation et la répartition des boutons utilisent `uinput`.

## Installation et empaquetage

- `packaging/install.sh` : installe le binaire, les icônes, le fichier `.desktop` et
  les traductions pour l'utilisateur courant (aucun `sudo`, tout dans `~/.local`).
- `packaging/flatpak/` : manifeste Flatpak.
- `packaging/appimage/` : recette AppImage.

App-id public : `io.github.f3nr1l.WinctrlConfig` (à figer au vrai slug du projet
avant publication).

## Licence

GPLv3 ou ultérieure (`GPL-3.0-or-later`).

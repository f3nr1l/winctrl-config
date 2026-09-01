//! `winwing-config` — bibliothèque de l'application de configuration des manches
//! WinWing URSA MINOR.
//!
//! Organisation en couches :
//!
//! - [`protocol`] : opcodes, identité des contrôleurs, carte des offsets flash,
//!   garde-fou d'identité (règle de sécurité n°1) et encodeurs de trame. Couche
//!   pure, sans I/O.
//! - [`enumerate`] : découverte des périphériques par parsing `/sys` (sans I/O
//!   hidraw).
//! - [`model`] : types de domaine, décodeurs purs (octets → humain), écritures
//!   gardées (sauvegarde → diff → garde-fou → écho → relecture) et le
//!   `trait Transport`.
//! - [`transport`] : implémentation hidraw du `trait Transport`.
//! - [`ui`] : interface GTK4 / libadwaita (derrière la feature `gui`).

pub mod enumerate;
pub mod i18n;
pub mod model;
pub mod protocol;
pub mod transport;

/// Sous-système entrée (côté OS, aucune écriture flash) : moniteur live evdev
/// ([`livemon`]) et moteur uinput remap/split ([`remap`]). Comme `transport`, ces
/// couches font de l'I/O (nix/libc) mais restent hors GTK et sans autre dépendance.
pub mod livemon;
pub mod remap;
/// Test direct du moteur de vibration (force feedback `FF_RUMBLE` du noyau).
/// Réversible, aucune écriture flash.
pub mod rumble;
/// Flashage du firmware (bootloader) — **irréversible**. Cœur pur (parsing `.wwtc`,
/// CRC, validation) testable ; pilote avec dry-run par défaut. Gate humain strict.
pub mod flash;
/// Courbe de réponse + inversion d'axe ([`axis_curve`]) : moteur **pur**
/// (reconstruction du `CurveData` de SimApp Pro), appliqué par [`remap`] sur le
/// device virtuel uinput. Aucune écriture flash.
pub mod axis_curve;
/// Persistance des remaps par appareil (JSON `serde_json`). Derrière la feature
/// `gui` : son unique consommateur est la page Remap, et `serde_json` est une dép
/// optionnelle tirée par `gui`.
#[cfg(feature = "gui")]
pub mod remap_store;
/// Persistance des courbes/inversions d'axe par appareil (JSON `serde_json`).
/// Derrière `gui` comme [`remap_store`], dont elle réutilise les helpers disque.
#[cfg(feature = "gui")]
pub mod axis_store;
/// Vérification en ligne de la dernière version de firmware (catalogue WinWing).
/// Lecture seule (aucun flash). Derrière `gui` (`serde_json`).
#[cfg(feature = "gui")]
pub mod firmware;
pub mod vibration;

/// UI GTK4/libadwaita (derrière la feature `gui`, activée par défaut). Le cœur
/// pur (`protocol`/`enumerate`/`model`) reste compilable sans elle
/// (`--no-default-features`) — seul `transport` tire `nix`.
#[cfg(feature = "gui")]
pub mod ui;

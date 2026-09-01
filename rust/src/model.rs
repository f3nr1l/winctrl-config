//! Modèle de domaine : types + décodeurs **purs** (octets flash → humain).
//!
//! On trouve ici :
//! - les types de domaine (`DecodedField`, `ControllerConfig`, `DeviceConfig`) ;
//! - le décodage octets→humain (`humanize`, `decode_field`, `twist_label`) ;
//! - le format des dumps (`format_dump`) ;
//! - le contrat `trait Transport` ;
//! - les écritures gardées qui composent le transport (lecture, dry-run/apply,
//!   calibration, sauvegarde, restauration).

use std::fmt;
use std::path::PathBuf;

use crate::enumerate::Controller;
use crate::protocol as p;

/// Région config = page 2 Ko lisible jusqu'à 0x7f4 (morte dès 0x7f8), mais toutes
/// les données utiles sont en 0x000–0x1dc. On dumpe jusqu'à 0x200 : couvre l'utile
/// et reste diffable avec les dumps de référence.
pub const DEFAULT_DUMP_END: u32 = 0x200;

/// Offset du mode de l'axe twist Z/Rz (poignée).
pub const Z_AXIS_MODE_OFFSET: u32 = 0xD8;

// --- Types de domaine -----------------------------------------------------

/// Un champ flash lu et rendu lisible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedField {
    pub offset: u32,
    /// Nom technique du champ (jamais traduit).
    pub name: &'static str,
    /// `None` = pas de réponse (offset hors région).
    pub raw: Option<[u8; 4]>,
    /// Valeur interprétée pour l'affichage.
    pub human: String,
    pub identity: bool,
}

impl DecodedField {
    /// Rendu hexa des 4 octets, ou `"-- -- -- --"` si absent.
    pub fn hex(&self) -> String {
        match &self.raw {
            Some(b) => p::hx(b),
            None => "-- -- -- --".to_string(),
        }
    }
}

/// Config décodée d'un contrôleur `(device, family)` à un instant t.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControllerConfig {
    pub device: u8,
    pub family: u8,
    pub model: &'static str,
    pub fields: Vec<DecodedField>,
    pub product_name: String,
    pub serial: String,
    /// Valeur brute de l'octet Z_AXIS_MODE (0xd8), si lu.
    pub twist_mode: Option<u8>,
}

impl ControllerConfig {
    /// Crée une config vide pour `(device, family)`, modèle résolu depuis la table.
    pub fn new(device: u8, family: u8) -> Self {
        ControllerConfig {
            device,
            family,
            model: p::controller_name(device, family).unwrap_or("?"),
            ..Default::default()
        }
    }

    /// Libellé (anglais source) du mode twist courant.
    pub fn twist_label(&self) -> String {
        match self.twist_mode {
            None => "unknown".to_string(),
            Some(mode) => p::z_mode_label(mode)
                .map(str::to_string)
                .unwrap_or_else(|| format!("?({mode:#04x})")),
        }
    }
}

/// Config de tous les contrôleurs d'un endpoint.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceConfig {
    pub hidraw: String,
    pub controllers: Vec<ControllerConfig>,
}

// --- Décodeurs purs (octets → humain) -------------------------------------

/// Interprétation courte d'un champ pour l'affichage. **Pure.**
///
/// Port de `_humanize` : mêmes cas, mêmes textes. Les libellés sont gardés
/// bruts (traduction gettext appliquée côté UI, cf. architecture §3).
pub fn humanize(field: &p::FlashField, raw: Option<&[u8; 4]>) -> String {
    let raw = match raw {
        None => return "(out of range / no response)".to_string(),
        Some(r) => r,
    };
    match field.name {
        "z_axis_mode" => {
            let mode = raw[0];
            let label = p::z_mode_label(mode)
                .map(str::to_string)
                .unwrap_or_else(|| format!("?({mode:#04x})"));
            format!("yaw = {label}")
        }
        "firmware_complete" => {
            if raw[0] == 1 {
                "Firmware complete".to_string()
            } else {
                "Incomplete".to_string()
            }
        }
        "restore_default" => {
            if raw[0] == 0xFF {
                "Inactive".to_string()
            } else {
                "Armed (reset on next restart)".to_string()
            }
        }
        "hardware_version" => {
            // u16 LE ; format vendeur = hi.lo hexa
            let v = raw[0] as u16 | ((raw[1] as u16) << 8);
            format!("HW {:02x}.{:02x}", v >> 8, v & 0xFF)
        }
        _ => p::hx(raw),
    }
}

/// Construit un `DecodedField` à partir d'un `FlashField` et de ses octets bruts.
/// **Pure** — l'I/O (lecture des octets) est faite en amont par le Transport.
pub fn decode_field(field: &p::FlashField, raw: Option<[u8; 4]>) -> DecodedField {
    DecodedField {
        offset: field.offset,
        name: field.name,
        human: humanize(field, raw.as_ref()),
        raw,
        identity: field.identity,
    }
}

/// Format des dumps de référence : `0xNNNN: xx xx xx xx` (diffable). **Pure.**
pub fn format_dump(rows: &[(u32, Option<[u8; 4]>)]) -> String {
    let mut out = String::new();
    for (off, v) in rows {
        let hex = match v {
            Some(b) => p::hx(b),
            None => "-- -- -- --".to_string(),
        };
        out.push_str(&format!("0x{off:04x}: {hex}\n"));
    }
    out
}

// --- Modes twist : noms CLI/UI <-> valeur d'octet (tables pures) -----------

/// Noms CLI/UI → valeur de l'octet Z_AXIS_MODE (protocole §6).
pub static TWIST_MODES: &[(&str, u8)] = &[
    ("buttons", p::Z_MODE_BUTTONS_ONLY),        // 0x00
    ("axis+buttons", p::Z_MODE_AXIS_AND_BUTTONS), // 0x01
    ("axis", p::Z_MODE_AXIS_ONLY),              // 0xff
];

/// Valeur d'octet du mode twist à partir de son nom CLI/UI.
pub fn twist_mode_from_name(name: &str) -> Option<u8> {
    TWIST_MODES.iter().find(|(n, _)| *n == name).map(|(_, v)| *v)
}

/// Nom CLI/UI d'un mode twist à partir de sa valeur d'octet.
pub fn twist_mode_name(value: u8) -> Option<&'static str> {
    TWIST_MODES.iter().find(|(_, v)| *v == value).map(|(n, _)| *n)
}

// --- Zones mortes : valeur à afficher -------------------------------------
// Une zone morte non réglée remonte la sentinelle `0xFFFFFFFF` ; affichée telle
// quelle (bornée au champ), elle donne « 65535 », trompeur. On ne retient donc
// qu'une valeur réellement posée, sinon l'UI affiche « désactivée ».

/// Zone morte d'axe de base (uint32) à afficher, ou `None` si aucune zone morte
/// n'est réellement posée (sentinelle, hors plage, ou 0).
pub fn base_deadzone_display(raw: Option<u32>) -> Option<u32> {
    match raw {
        Some(v) if (1..=65535).contains(&v) => Some(v),
        _ => None,
    }
}

/// Zone morte du twist (octet) à afficher, ou `None` si aucune n'est posée.
pub fn twist_deadzone_display(raw: Option<u8>) -> Option<u8> {
    match raw {
        Some(v) if (1..=p::DEADZONE_TWIST_MAX).contains(&v) => Some(v),
        _ => None,
    }
}

// --- Contrat Transport (§1.1) — signatures seules -------------------------

/// Erreur d'une opération de transport.
#[derive(Debug)]
pub enum TransportError {
    /// Refus du garde-fou d'écriture (identité / offset interdit).
    Guard(p::WriteGuardError),
    /// Erreur d'I/O sous-jacente.
    Io(std::io::Error),
}

impl fmt::Display for TransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TransportError::Guard(e) => write!(f, "{e}"),
            TransportError::Io(e) => write!(f, "erreur d'I/O : {e}"),
        }
    }
}

impl std::error::Error for TransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            TransportError::Guard(e) => Some(e),
            TransportError::Io(e) => Some(e),
        }
    }
}

impl From<p::WriteGuardError> for TransportError {
    fn from(e: p::WriteGuardError) -> Self {
        TransportError::Guard(e)
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}

/// I/O hidraw report-id-2 vers un endpoint WinWing (poignée + base).
///
/// **Contrat défini, non implémenté dans cet incrément.** Un endpoint porte deux
/// contrôleurs logiques : on adresse l'un ou l'autre par `(device, family)` à
/// chaque appel. Le format de trame et l'écho-oracle sont fixés par le module
/// `protocol` — l'implémentation ne change que la mécanique d'I/O.
///
/// Invariants portés du prototype (`transport.py`) :
/// - **écho-oracle** : le device renvoie la trame acceptée (opcode en `[6]`,
///   bit `0x10` sur l'octet family) ; l'absence d'écho vaut rejet ;
/// - **mono-écrivain** : au plus un handle ouvert par endpoint ;
/// - **garde-fou d'écriture** : `write_cfg` applique [`p::guard_write`] avant
///   d'émettre (règle de sécurité n°1, §2.4) ;
/// - **appels potentiellement bloquants** : jamais sur le thread UI (§1.4).
pub trait Transport: Sized {
    /// Ouvre `/dev/hidrawN` en R/W non bloquant.
    // impl : incrément Transport, hidapi ou nix
    fn open(path: &str) -> std::io::Result<Self>;

    /// Libère le descripteur.
    // impl : incrément Transport, hidapi ou nix
    fn close(self);

    /// Lit 4 octets de config flash à `offset`. `None` si pas d'écho
    /// (non destructif).
    // impl : incrément Transport, hidapi ou nix
    fn read_cfg(&mut self, device: u8, family: u8, offset: u32) -> Option<[u8; 4]>;

    /// Émet une requête d'identité/mode (REQUEST_DEVICE_* **uniquement** :
    /// HW/FW/SN/MODE) et rend la charge utile, ou `None`.
    // impl : incrément Transport, hidapi ou nix
    fn request(&mut self, device: u8, family: u8, opcode: u8) -> Option<Vec<u8>>;

    /// Écrit 4 octets à `offset`, **gardé identité** ([`p::guard_write`]).
    /// `allow_identity` n'est jamais posé par l'app. Rend `(trame émise, écho)`.
    // impl : incrément Transport, hidapi ou nix
    fn write_cfg(
        &mut self,
        device: u8,
        family: u8,
        offset: u32,
        data: [u8; 4],
        allow_identity: bool,
    ) -> Result<(p::Frame, Option<Vec<u8>>), TransportError>;

    /// `DEVICE_RESTART` : le contrôleur relit ses drapeaux (ré-énumère l'USB).
    /// Rend `(trame émise, écho)`.
    // impl : incrément Transport, hidapi ou nix
    fn restart(&mut self, device: u8, family: u8) -> (p::Frame, Option<Vec<u8>>);
}

// --- Logique de LECTURE (compose Transport + décodeurs) -------------------
// Générique sur `T: Transport` : ne dépend d'aucun transport concret (pas de
// cycle de couches). Le binaire choisit l'implémentation (HidrawTransport).
// LECTURE SEULE : aucun write applicatif ici (backup→diff→vérif = lot suivant,
// gate humain).

/// Lit la plage `[start, end]` par blocs de 4 octets ; s'arrête au 1er trou
/// (offset sans écho). **Lecture non destructive.**
fn read_region<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    start: u32,
    end: u32,
) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut off = start;
    while off <= end {
        match t.read_cfg(device, family, off) {
            Some(v) => buf.extend_from_slice(&v),
            None => break,
        }
        off += 4;
    }
    buf
}

/// Lit tous les champs connus d'un contrôleur et les décode (**read-only**).
///
/// Les champs marqués `grip_only` ne sont lus/affichés que pour la poignée
/// (family 0xbf) : sur la base, ces offsets ont un tout autre sens (décodage
/// family-aware). Port de `model.py::read_controller`.
pub fn read_controller<T: Transport>(t: &mut T, device: u8, family: u8) -> ControllerConfig {
    let mut cfg = ControllerConfig::new(device, family);
    for field in p::FLASH_FIELDS {
        if field.grip_only && family != p::FAMILY_GRIP {
            continue;
        }
        let raw = t.read_cfg(device, family, field.offset);
        if field.name == "z_axis_mode" {
            if let Some(r) = raw {
                cfg.twist_mode = Some(r[0]);
            }
        }
        cfg.fields.push(decode_field(field, raw));
    }
    // Nom produit : ASCII jusqu'au premier NUL. Numéro de série : binaire -> hexa.
    let name_raw = read_region(t, device, family, 0x5C, 0x84);
    let name_end = name_raw.iter().position(|&b| b == 0).unwrap_or(name_raw.len());
    cfg.product_name = String::from_utf8_lossy(&name_raw[..name_end]).trim().to_string();
    let ser = read_region(t, device, family, 0x9C, 0xA4);
    cfg.serial = ser.iter().map(|b| format!("{b:02x}")).collect();
    cfg
}

/// Ouvre l'endpoint une fois (via `T::open`) et lit chaque contrôleur listé.
/// **Read-only.** Générique sur le transport ; le binaire fixe `T`.
/// Port de `model.py::read_device`.
pub fn read_device<T: Transport>(
    path: &str,
    controllers: &[Controller],
) -> std::io::Result<DeviceConfig> {
    let mut t = T::open(path)?;
    let mut out = DeviceConfig {
        hidraw: path.to_string(),
        controllers: Vec::new(),
    };
    for c in controllers {
        out.controllers.push(read_controller(&mut t, c.device, c.family));
    }
    t.close();
    Ok(out)
}

// --- Instantané complet (config + états annexes) --------------------------
// Regroupe, en UNE session de transport par endpoint, tout ce dont les pages
// read-only ont besoin. S'étoffe au fil des pages (région de calibration…)
// sans jamais faire relire le device par les pages elles-mêmes.

/// État LED persistant de la base (rétroéclairage). Valeurs `None` = non lues.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BaseLedState {
    /// Luminosité persistée (`0xEC` octet[0]) ; `0xff` (non défini) rendu comme 0.
    pub brightness: Option<u8>,
    /// Respiration active (`0xF8` octet[0] == `BREATHING_VALUE`=0) sinon fixe.
    pub breathing: Option<bool>,
}

/// Données de calibration d'un contrôleur (read-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerCalib {
    pub device: u8,
    pub family: u8,
    pub model: &'static str,
    /// Noms des axes calibrables de ce contrôleur.
    pub axes: Vec<&'static str>,
    /// Région 0xC8–0xF8 lue (offset -> 4 octets) — remplie pour la poignée ;
    /// vide pour la base (ces offsets y recouvrent d'autres champs).
    pub region: Vec<(u32, Option<[u8; 4]>)>,
}

/// Instantané d'un manche : config décodée + état LED base + calibration.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceSnapshot {
    pub config: DeviceConfig,
    /// Présent si l'endpoint porte un contrôleur de base (family 0xbb).
    pub base_led: Option<BaseLedState>,
    /// Une entrée par contrôleur.
    pub calib: Vec<ControllerCalib>,
    /// Mode 4x32 de la base (`0xC8` octet 0 == 1).
    pub base_4x32: Option<bool>,
    /// Zone morte X de la base (`0xD8` uint32 LE).
    pub deadzone_x: Option<u32>,
    /// Zone morte Y de la base (`0xE8` uint32 LE).
    pub deadzone_y: Option<u32>,
    /// Zone morte du twist Rz de la poignée (`0x104` octet 2).
    pub twist_deadzone: Option<u8>,
}

/// Lit l'état LED persistant de la base (`0xEC` luminosité, `0xF8` respiration).
/// **Read-only** (deux `READ_CFG_DATA`).
fn read_base_led<T: Transport>(t: &mut T, device: u8, family: u8) -> BaseLedState {
    let bl = t.read_cfg(device, family, p::BASE_BACKLIGHT_OFFSET);
    let br = t.read_cfg(device, family, p::BASE_BREATHING_OFFSET);
    BaseLedState {
        brightness: bl.map(|b| if b[0] == 0xFF { 0 } else { b[0] }),
        breathing: br.map(|b| b[0] == p::BREATHING_VALUE),
    }
}

/// Ouvre l'endpoint UNE fois et lit tout ce que les pages read-only affichent :
/// config de chaque contrôleur + état LED de la base. **Read-only.** Générique
/// sur le transport ; le binaire fixe `T`.
pub fn read_snapshot<T: Transport>(
    path: &str,
    controllers: &[Controller],
) -> std::io::Result<DeviceSnapshot> {
    let mut t = T::open(path)?;
    let mut config = DeviceConfig {
        hidraw: path.to_string(),
        controllers: Vec::new(),
    };
    let mut base_led = None;
    let mut base_4x32 = None;
    let mut deadzone_x = None;
    let mut deadzone_y = None;
    let mut twist_deadzone = None;
    let mut calib = Vec::new();
    for c in controllers {
        config.controllers.push(read_controller(&mut t, c.device, c.family));
        if c.family == p::FAMILY_BASE {
            base_led = Some(read_base_led(&mut t, c.device, c.family));
            base_4x32 = t
                .read_cfg(c.device, c.family, p::BASE_4X32_OFFSET)
                .map(|v| v[0] == 1);
            deadzone_x = t
                .read_cfg(c.device, c.family, p::BASE_DEADZONE_X_OFFSET)
                .map(u32::from_le_bytes);
            deadzone_y = t
                .read_cfg(c.device, c.family, p::BASE_DEADZONE_Y_OFFSET)
                .map(u32::from_le_bytes);
        }
        if c.family == p::FAMILY_GRIP {
            twist_deadzone = t
                .read_cfg(c.device, c.family, p::GRIP_DEADZONE_TWIST_OFFSET)
                .map(|v| v[2]);
        }
        let axes = p::calibration_indexes(c.family)
            .iter()
            .filter_map(|&i| p::axis_index_name(i))
            .collect();
        // Région 0xC8–0xF8 : lue pour la poignée (calibration) ; sur la base ces
        // offsets recouvrent d'autres champs -> non affichés comme calibration.
        let region = if c.family == p::FAMILY_GRIP {
            p::calib_region_offsets()
                .into_iter()
                .map(|off| (off, t.read_cfg(c.device, c.family, off)))
                .collect()
        } else {
            Vec::new()
        };
        calib.push(ControllerCalib {
            device: c.device,
            family: c.family,
            model: p::controller_name(c.device, c.family).unwrap_or("?"),
            axes,
            region,
        });
    }
    t.close();
    Ok(DeviceSnapshot {
        config,
        base_led,
        calib,
        base_4x32,
        deadzone_x,
        deadzone_y,
        twist_deadzone,
    })
}

// --- Écritures flash gardées (backup → diff-only → guard → écho → relecture) ---
// Discipline ARCHITECTURE §6, portée du prototype (set_twist_mode). `dry_run`
// n'émet RIEN et rend la TRAME qui serait émise (preuve). L'apply réel est
// déclenché par l'utilisateur (gate). Garde-fou identité/interdit dans write_cfg.

/// Compte rendu d'une écriture (ou de son plan en dry-run).
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub offset: u32,
    pub old: Option<[u8; 4]>,
    pub new: [u8; 4],
    /// Trame WRITE_CFG_DATA (émise en apply, seulement planifiée en dry-run).
    pub frame: p::Frame,
    /// `old != new`.
    pub changes: bool,
    pub dry_run: bool,
    /// Une écriture a réellement été envoyée.
    pub emitted: bool,
    /// Écho d'acceptation reçu.
    pub echo_ok: bool,
    pub readback: Option<[u8; 4]>,
    /// Relecture == `new` (ou rien à écrire).
    pub verified: bool,
    /// Déjà à la valeur : aucune écriture émise.
    pub skipped: bool,
    pub backup: Option<PathBuf>,
}

/// Répertoire de sauvegarde (`~/.local/share/winctrl/backups`).
fn backups_dir() -> std::io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .ok_or_else(|| std::io::Error::other("HOME introuvable"))?;
    Ok(PathBuf::from(home).join(".local/share/winctrl/backups"))
}

/// Migration douce des données utilisateur : au premier lancement, si le nouveau
/// dossier `~/.local/share/winctrl` n'existe pas encore mais que l'ancien
/// `~/.local/share/winwing` existe, on le renomme pour préserver les réglages,
/// sauvegardes et profils déjà enregistrés. Best-effort (silencieux si échec).
pub fn migrate_legacy_data_dir() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let base = PathBuf::from(home).join(".local/share");
    let old = base.join("winwing");
    let new = base.join("winctrl");
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

/// Dump horodaté de la config d'un contrôleur (0x00–0x1dc) → fichier diffable.
/// `ts` = horodatage fourni par l'appelant (le cœur n'a pas de lib de dates).
pub fn backup_controller<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    ts: &str,
) -> std::io::Result<PathBuf> {
    let dir = backups_dir()?;
    std::fs::create_dir_all(&dir)?;
    let mut rows: Vec<(u32, Option<[u8; 4]>)> = Vec::new();
    let mut off = 0u32;
    while off <= 0x1DC {
        rows.push((off, t.read_cfg(device, family, off)));
        off += 4;
    }
    let model = p::controller_name(device, family).unwrap_or("ctrl");
    let path = dir.join(format!("{model}-{family:02x}{device:02x}-{ts}.txt"));
    std::fs::write(&path, format_dump(&rows))?;
    Ok(path)
}

/// Écriture flash gardée d'UN octet à `offset` (préserve les autres octets) —
/// **diff-only**. `dry_run=true` n'émet RIEN et rend la trame planifiée ; en
/// apply, `ts` déclenche un backup horodaté avant l'écriture, puis écho +
/// relecture. Garde-fou identité/interdit appliqué par `write_cfg`.
// Chaque paramètre est essentiel (transport + adresse (dev/fam/offset/octet) +
// valeur + mode dry-run/backup) ; les regrouper n'apporterait rien.
#[allow(clippy::too_many_arguments)]
pub fn write_flash_byte<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    offset: u32,
    byte_index: usize,
    value: u8,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let old = t.read_cfg(device, family, offset);
    let cur = old.ok_or_else(|| {
        TransportError::Io(std::io::Error::other(format!(
            "lecture de {offset:#04x} impossible (pas d'écho)"
        )))
    })?;
    // diff-only : ne change que l'octet visé, préserve les autres.
    let mut new = cur;
    new[byte_index] = value;
    let mut args = p::offset_bytes(offset).to_vec();
    args.extend_from_slice(&new);
    let frame = p::build_frame(device, family, p::OP_WRITE_CFG_DATA, &args);
    let changes = cur != new;
    let mut out = WriteOutcome {
        offset,
        old,
        new,
        frame,
        changes,
        dry_run,
        emitted: false,
        echo_ok: false,
        readback: None,
        verified: false,
        skipped: false,
        backup: None,
    };
    if dry_run {
        out.verified = !changes; // rien à écrire = déjà bon
        return Ok(out);
    }
    if !changes {
        out.skipped = true;
        out.verified = true;
        return Ok(out);
    }
    if let Some(ts) = ts {
        out.backup = backup_controller(t, device, family, ts).ok();
    }
    let (_f, echo) = t.write_cfg(device, family, offset, new, false)?;
    out.emitted = true;
    out.echo_ok = echo.is_some();
    let rb = t.read_cfg(device, family, offset);
    out.readback = rb;
    out.verified = rb == Some(new);
    Ok(out)
}

/// Mode de l'axe twist Z (`0xD8`, octet 0) — diff-only (préserve la vitesse).
pub fn set_twist_mode<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    mode: u8,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    write_flash_byte(t, device, family, Z_AXIS_MODE_OFFSET, 0, mode, dry_run, ts)
}

/// Luminosité PERSISTÉE de la base (`0xEC`, octet 0). Diff-only. (Le réglage
/// LIVE non persistant passe par `HidrawTransport::set_led`, opcode 0x49.)
pub fn set_backlight_persist<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    value: u8,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    write_flash_byte(t, device, family, p::BASE_BACKLIGHT_OFFSET, 0, value, dry_run, ts)
}

/// Mode d'éclairage (`0xF8`, octet 0) : respiration (`BREATHING_VALUE`=0) ou
/// fixe (`STATIC_VALUE`=1). Diff-only.
pub fn set_breathing<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    breathing_on: bool,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let value = if breathing_on {
        p::BREATHING_VALUE
    } else {
        p::STATIC_VALUE
    };
    write_flash_byte(t, device, family, p::BASE_BREATHING_OFFSET, 0, value, dry_run, ts)
}

/// Écriture flash gardée d'un MOT complet (4 octets) à `offset` — diff-only.
pub fn write_flash_word<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    offset: u32,
    data: [u8; 4],
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let old = t.read_cfg(device, family, offset);
    let cur = old.ok_or_else(|| {
        TransportError::Io(std::io::Error::other(format!(
            "lecture de {offset:#04x} impossible (pas d'écho)"
        )))
    })?;
    let new = data;
    let mut args = p::offset_bytes(offset).to_vec();
    args.extend_from_slice(&new);
    let frame = p::build_frame(device, family, p::OP_WRITE_CFG_DATA, &args);
    let changes = cur != new;
    let mut out = WriteOutcome {
        offset,
        old,
        new,
        frame,
        changes,
        dry_run,
        emitted: false,
        echo_ok: false,
        readback: None,
        verified: false,
        skipped: false,
        backup: None,
    };
    if dry_run {
        out.verified = !changes;
        return Ok(out);
    }
    if !changes {
        out.skipped = true;
        out.verified = true;
        return Ok(out);
    }
    if let Some(ts) = ts {
        out.backup = backup_controller(t, device, family, ts).ok();
    }
    let (_f, echo) = t.write_cfg(device, family, offset, new, false)?;
    out.emitted = true;
    out.echo_ok = echo.is_some();
    let rb = t.read_cfg(device, family, offset);
    out.readback = rb;
    out.verified = rb == Some(new);
    Ok(out)
}

// --- D3 : zones mortes -----------------------------------------------------
/// Zone morte d'un axe de base (uint32 LE) : X=`0xD8`, Y=`0xE8`. Diff-only.
pub fn set_deadzone_base<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    y_axis: bool,
    value: u32,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let offset = if y_axis {
        p::BASE_DEADZONE_Y_OFFSET
    } else {
        p::BASE_DEADZONE_X_OFFSET
    };
    write_flash_word(t, device, family, offset, value.to_le_bytes(), dry_run, ts)
}

/// Zone morte du twist Rz : octet[2] de `0x104` (poignée), bornée à 0..30. Diff-only.
pub fn set_deadzone_twist<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    value: u8,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let v = value.min(p::DEADZONE_TWIST_MAX);
    write_flash_byte(t, device, family, p::GRIP_DEADZONE_TWIST_OFFSET, 2, v, dry_run, ts)
}

// --- D4 : mode 4x32 --------------------------------------------------------
/// Mode 4x32 de la base (`0xC8`, octet 0 = 0/1). Un REDÉMARRAGE est requis pour
/// prendre effet (`restart=true` l'émet après une écriture réelle). Diff-only + backup.
pub fn set_4x32<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    enabled: bool,
    restart: bool,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let out = write_flash_byte(
        t,
        device,
        family,
        p::BASE_4X32_OFFSET,
        0,
        u8::from(enabled),
        dry_run,
        ts,
    )?;
    if !dry_run && restart && out.emitted {
        let _ = t.restart(device, family);
    }
    Ok(out)
}

// --- D5 : calibration (lecture région ; l'ARMEMENT 0x47/0x48 est côté UI) ---
/// Lit la région de calibration 0xC8–0xF8 (offset -> 4 octets|None). Read-only.
pub fn read_calib_region<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
) -> Vec<(u32, Option<[u8; 4]>)> {
    p::calib_region_offsets()
        .into_iter()
        .map(|off| (off, t.read_cfg(device, family, off)))
        .collect()
}

// --- D6 : restore-default --------------------------------------------------
/// Réinitialisation usine : drapeau `0xB4`=[1,0,0,0] + `DEVICE_RESTART`.
/// TRÈS large (remet TOUTE la config). `dry_run` rend la trame sans écrire ;
/// en apply : backup complet → write → restart. À N'exposer qu'avec double
/// confirmation dans l'UI.
pub fn restore_default<T: Transport>(
    t: &mut T,
    device: u8,
    family: u8,
    dry_run: bool,
    ts: Option<&str>,
) -> Result<WriteOutcome, TransportError> {
    let out = write_flash_word(
        t,
        device,
        family,
        p::RESTORE_DEFAULT_OFFSET,
        p::RESTORE_DEFAULT_DATA,
        dry_run,
        ts,
    )?;
    if !dry_run && out.emitted {
        let _ = t.restart(device, family);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_dump_matches_reference_style() {
        let rows = [
            (0x00u32, Some([0, 0, 0, 0])),
            (0x04, Some([1, 0, 0, 0])),
            (0x08, None),
        ];
        let out = format_dump(&rows);
        assert_eq!(
            out,
            "0x0000: 00 00 00 00\n0x0004: 01 00 00 00\n0x0008: -- -- -- --\n"
        );
    }

    // Retrouve un FlashField de la table par nom, pour tester humanize.
    fn field(name: &str) -> &'static p::FlashField {
        p::FLASH_FIELDS.iter().find(|f| f.name == name).unwrap()
    }

    #[test]
    fn humanize_none_is_out_of_region() {
        assert_eq!(
            humanize(field("z_axis_mode"), None),
            "(out of range / no response)"
        );
    }

    #[test]
    fn humanize_twist_modes() {
        assert_eq!(
            humanize(field("z_axis_mode"), Some(&[0x00, 0xff, 0, 0])),
            "yaw = Buttons only"
        );
        assert_eq!(
            humanize(field("z_axis_mode"), Some(&[0xff, 0xff, 0, 0])),
            "yaw = Axis only"
        );
        // valeur inconnue -> ?(0x02)
        assert_eq!(
            humanize(field("z_axis_mode"), Some(&[0x02, 0, 0, 0])),
            "yaw = ?(0x02)"
        );
    }

    #[test]
    fn humanize_firmware_and_restore_and_hw() {
        assert_eq!(humanize(field("firmware_complete"), Some(&[1, 0, 0, 0])), "Firmware complete");
        assert_eq!(humanize(field("firmware_complete"), Some(&[0, 0, 0, 0])), "Incomplete");
        assert_eq!(humanize(field("restore_default"), Some(&[0xff, 0, 0, 0])), "Inactive");
        assert_eq!(
            humanize(field("restore_default"), Some(&[1, 0, 0, 0])),
            "Armed (reset on next restart)"
        );
        // HW 01.02 depuis u16 LE 0x0201
        assert_eq!(humanize(field("hardware_version"), Some(&[0x01, 0x02, 0, 0])), "HW 02.01");
    }

    #[test]
    fn decoded_field_hex_and_identity() {
        let f = decode_field(field("type_id"), Some([0xde, 0xad, 0xbe, 0xef]));
        assert_eq!(f.hex(), "de ad be ef");
        assert!(f.identity);
        assert_eq!(f.offset, 0x08);
        let g = decode_field(field("firmware_complete"), None);
        assert_eq!(g.hex(), "-- -- -- --");
        assert!(!g.identity);
    }

    #[test]
    fn controller_config_twist_label() {
        let mut cfg = ControllerConfig::new(0x0A, p::FAMILY_GRIP);
        assert_eq!(cfg.model, "JGRIP_F1_R");
        assert_eq!(cfg.twist_label(), "unknown");
        cfg.twist_mode = Some(p::Z_MODE_AXIS_AND_BUTTONS);
        assert_eq!(cfg.twist_label(), "Axis and buttons");
        cfg.twist_mode = Some(0x07);
        assert_eq!(cfg.twist_label(), "?(0x07)");
    }

    #[test]
    fn twist_mode_name_roundtrip() {
        assert_eq!(twist_mode_from_name("axis"), Some(0xFF));
        assert_eq!(twist_mode_from_name("buttons"), Some(0x00));
        assert_eq!(twist_mode_name(p::Z_MODE_AXIS_AND_BUTTONS), Some("axis+buttons"));
        assert_eq!(twist_mode_from_name("nope"), None);
    }

    // Le garde-fou est réutilisé tel quel par le futur write_cfg via
    // TransportError::Guard — on vérifie ici la conversion d'erreur.
    #[test]
    fn transport_error_wraps_guard() {
        let e = p::guard_write(0x08, false).unwrap_err();
        let te: TransportError = e.into();
        assert!(matches!(te, TransportError::Guard(p::WriteGuardError::Identity(0x08))));
        assert_eq!(te.to_string(), e.to_string());
    }

    // Le contrat compile : un transport factice implémente le trait (aucune I/O
    // réelle ici — l'implémentation vivante arrive à l'incrément Transport).
    struct DummyTransport;
    impl Transport for DummyTransport {
        fn open(_path: &str) -> std::io::Result<Self> {
            Ok(DummyTransport)
        }
        fn close(self) {}
        fn read_cfg(&mut self, _d: u8, _f: u8, _o: u32) -> Option<[u8; 4]> {
            None
        }
        fn request(&mut self, _d: u8, _f: u8, _op: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_cfg(
            &mut self,
            device: u8,
            family: u8,
            offset: u32,
            data: [u8; 4],
            allow_identity: bool,
        ) -> Result<(p::Frame, Option<Vec<u8>>), TransportError> {
            // même discipline que transport.py : garde d'abord, trame ensuite
            p::guard_write(offset, allow_identity)?;
            let mut args = p::offset_bytes(offset).to_vec();
            args.extend_from_slice(&data);
            Ok((p::build_frame(device, family, p::OP_WRITE_CFG_DATA, &args), None))
        }
        fn restart(&mut self, device: u8, family: u8) -> (p::Frame, Option<Vec<u8>>) {
            (p::build_frame(device, family, p::OP_DEVICE_RESTART, &[]), None)
        }
    }

    #[test]
    fn dummy_transport_honours_guard() {
        let mut t = DummyTransport::open("/dev/null").unwrap();
        // écriture ordinaire : trame write cfg bien formée
        let (frame, echo) = t.write_cfg(0x20, p::FAMILY_BASE, 0xC8, [1, 0, 0, 0], false).unwrap();
        assert_eq!(p::hx(&frame), "02 20 bb 00 00 08 06 c8 00 00 01 00 00 00");
        assert!(echo.is_none());
        // offset d'identité : refusé
        assert!(matches!(
            t.write_cfg(0x20, p::FAMILY_BASE, 0x5C, [0; 4], false),
            Err(TransportError::Guard(p::WriteGuardError::Identity(0x5C)))
        ));
        t.close();
    }

    // Transport en mémoire : flash factice (device,family,offset) -> 4 octets,
    // pour tester read_controller/read_device sans matériel.
    struct FakeTransport {
        flash: std::collections::HashMap<(u8, u8, u32), [u8; 4]>,
    }
    impl Transport for FakeTransport {
        fn open(_path: &str) -> std::io::Result<Self> {
            Ok(FakeTransport { flash: std::collections::HashMap::new() })
        }
        fn close(self) {}
        fn read_cfg(&mut self, d: u8, f: u8, o: u32) -> Option<[u8; 4]> {
            self.flash.get(&(d, f, o)).copied()
        }
        fn request(&mut self, _d: u8, _f: u8, _op: u8) -> Option<Vec<u8>> {
            None
        }
        fn write_cfg(
            &mut self,
            device: u8,
            family: u8,
            offset: u32,
            data: [u8; 4],
            allow_identity: bool,
        ) -> Result<(p::Frame, Option<Vec<u8>>), TransportError> {
            p::guard_write(offset, allow_identity)?;
            // Stocke la valeur pour que la relecture reflète l'écriture, et
            // simule un écho d'acceptation.
            self.flash.insert((device, family, offset), data);
            let mut args = p::offset_bytes(offset).to_vec();
            args.extend_from_slice(&data);
            let frame = p::build_frame(device, family, p::OP_WRITE_CFG_DATA, &args);
            Ok((frame, Some(vec![p::OP_WRITE_CFG_DATA])))
        }
        fn restart(&mut self, device: u8, family: u8) -> (p::Frame, Option<Vec<u8>>) {
            (p::build_frame(device, family, p::OP_DEVICE_RESTART, &[]), None)
        }
    }

    #[test]
    fn twist_dry_run_emits_nothing_and_diff_only() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x0A, p::FAMILY_GRIP, 0xD8), [0xFF, 0xAA, 0xBB, 0xCC]);
        let out = set_twist_mode(&mut t, 0x0A, p::FAMILY_GRIP, 0x00, true, None).unwrap();
        assert!(out.dry_run && !out.emitted);
        assert!(out.changes);
        assert_eq!(out.new, [0x00, 0xAA, 0xBB, 0xCC]); // vitesse [1..4] préservée
        // Trame planifiée == trame connue-bonne (format prototype set_twist_mode).
        assert_eq!(p::hx(&out.frame), "02 0a bf 00 00 08 06 d8 00 00 00 aa bb cc");
        // Aucune écriture : flash inchangée.
        assert_eq!(t.flash.get(&(0x0A, p::FAMILY_GRIP, 0xD8)), Some(&[0xFF, 0xAA, 0xBB, 0xCC]));
    }

    #[test]
    fn twist_apply_diff_only_verified() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x0A, p::FAMILY_GRIP, 0xD8), [0xFF, 0x11, 0x22, 0x33]);
        let out = set_twist_mode(&mut t, 0x0A, p::FAMILY_GRIP, 0x01, false, None).unwrap();
        assert!(out.emitted && out.echo_ok && out.verified && !out.skipped);
        // seul l'octet de mode change ; vitesse préservée.
        assert_eq!(t.flash.get(&(0x0A, p::FAMILY_GRIP, 0xD8)), Some(&[0x01, 0x11, 0x22, 0x33]));
    }

    #[test]
    fn twist_skips_when_already_at_value() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x0A, p::FAMILY_GRIP, 0xD8), [0x01, 0, 0, 0]);
        let out = set_twist_mode(&mut t, 0x0A, p::FAMILY_GRIP, 0x01, false, None).unwrap();
        assert!(out.skipped && !out.emitted && out.verified);
    }

    #[test]
    fn backlight_persist_diff_only_frame() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x20, p::FAMILY_BASE, 0xEC), [0x00, 0xAA, 0xBB, 0xCC]);
        let out = set_backlight_persist(&mut t, 0x20, p::FAMILY_BASE, 200, true, None).unwrap();
        assert!(out.dry_run && !out.emitted);
        assert_eq!(out.new, [200, 0xAA, 0xBB, 0xCC]); // [1..3] préservés
        assert_eq!(p::hx(&out.frame), "02 20 bb 00 00 08 06 ec 00 00 c8 aa bb cc");
    }

    #[test]
    fn breathing_writes_0_static_writes_1() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x20, p::FAMILY_BASE, 0xF8), [0xFF, 0, 0, 0]);
        let on = set_breathing(&mut t, 0x20, p::FAMILY_BASE, true, false, None).unwrap();
        assert_eq!(t.flash[&(0x20, p::FAMILY_BASE, 0xF8)][0], p::BREATHING_VALUE); // 0
        let off = set_breathing(&mut t, 0x20, p::FAMILY_BASE, false, false, None).unwrap();
        assert_eq!(t.flash[&(0x20, p::FAMILY_BASE, 0xF8)][0], p::STATIC_VALUE); // 1
        assert!(on.verified && off.verified);
    }

    #[test]
    fn backlight_live_frame_matches_known_good() {
        // SET_LEDX (0x49) idx0 valeur 200 (envoi LIVE, fire-and-forget).
        let f = p::build_frame(0x20, p::FAMILY_BASE, p::OP_SET_LEDX, &[p::LED_INDEX_BACKLIGHT, 200]);
        assert_eq!(&p::hx(&f)[..26], "02 20 bb 00 00 03 49 00 c8");
    }

    #[test]
    fn deadzone_display_hides_sentinel() {
        // Sentinelle « non réglée » (0xFFFFFFFF) et 0 => aucune valeur affichée.
        assert_eq!(base_deadzone_display(Some(0xFFFF_FFFF)), None);
        assert_eq!(base_deadzone_display(Some(0)), None);
        assert_eq!(base_deadzone_display(None), None);
        // Valeur réellement posée => affichée.
        assert_eq!(base_deadzone_display(Some(1200)), Some(1200));
        assert_eq!(base_deadzone_display(Some(65535)), Some(65535));
        // Twist : plage 1..=30, sinon rien.
        assert_eq!(twist_deadzone_display(Some(0xFF)), None);
        assert_eq!(twist_deadzone_display(Some(0)), None);
        assert_eq!(twist_deadzone_display(Some(12)), Some(12));
    }

    #[test]
    fn deadzone_base_word_le_frame() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x20, p::FAMILY_BASE, 0xD8), [0, 0, 0, 0]);
        let out = set_deadzone_base(&mut t, 0x20, p::FAMILY_BASE, false, 0x1234, true, None).unwrap();
        assert_eq!(out.new, [0x34, 0x12, 0x00, 0x00]); // uint32 LE
        assert_eq!(p::hx(&out.frame), "02 20 bb 00 00 08 06 d8 00 00 34 12 00 00");
        assert!(out.dry_run && !out.emitted);
    }

    #[test]
    fn deadzone_twist_byte2_clamped_and_preserved() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x0A, p::FAMILY_GRIP, 0x104), [0xAA, 0xBB, 0xFF, 0xDD]);
        let out = set_deadzone_twist(&mut t, 0x0A, p::FAMILY_GRIP, 99, false, None).unwrap();
        let v = t.flash[&(0x0A, p::FAMILY_GRIP, 0x104)];
        assert_eq!(v[2], p::DEADZONE_TWIST_MAX); // 99 -> 30
        assert_eq!(&v[..2], &[0xAA, 0xBB]); // préservés
        assert_eq!(v[3], 0xDD);
        assert!(out.verified);
    }

    #[test]
    fn fourx32_frame_and_diff() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x20, p::FAMILY_BASE, 0xC8), [0, 0, 0, 0]);
        let out = set_4x32(&mut t, 0x20, p::FAMILY_BASE, true, false, true, None).unwrap();
        assert_eq!(out.new, [1, 0, 0, 0]);
        assert_eq!(p::hx(&out.frame), "02 20 bb 00 00 08 06 c8 00 00 01 00 00 00");
    }

    #[test]
    fn restore_default_frame() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x0A, p::FAMILY_GRIP, 0xB4), [0xFF, 0xFF, 0xFF, 0xFF]);
        let out = restore_default(&mut t, 0x0A, p::FAMILY_GRIP, true, None).unwrap();
        assert_eq!(out.new, [1, 0, 0, 0]);
        assert_eq!(p::hx(&out.frame), "02 0a bf 00 00 08 06 b4 00 00 01 00 00 00");
        assert!(out.dry_run && !out.emitted);
    }

    #[test]
    fn calibration_frames_47_48() {
        // Trames d'armement (l'hôte n'écrit PAS la calibration ; le firmware si).
        let start = p::build_frame(0x0A, p::FAMILY_GRIP, p::OP_CALIBRATION_START, &[2]);
        let finish = p::build_frame(0x0A, p::FAMILY_GRIP, p::OP_CALIBRATION_FINISH, &[2]);
        assert_eq!(&p::hx(&start)[..23], "02 0a bf 00 00 02 47 02");
        assert_eq!(&p::hx(&finish)[..23], "02 0a bf 00 00 02 48 02");
    }

    #[test]
    fn read_controller_decodes_grip_fields() {
        let gdev = 0x0A;
        let gfam = p::FAMILY_GRIP;
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((gdev, gfam, 0x04), [1, 0, 0, 0]); // firmware_complete
        t.flash.insert((gdev, gfam, 0xD8), [0xFF, 0xFF, 0, 0]); // z_axis_mode = axe seul
        // nom produit "UR" à 0x5C puis NUL
        t.flash.insert((gdev, gfam, 0x5C), [b'U', b'R', 0, 0]);
        // série : un mot à 0x9C
        t.flash.insert((gdev, gfam, 0x9C), [0xde, 0xad, 0xbe, 0xef]);

        let cfg = read_controller(&mut t, gdev, gfam);
        assert_eq!(cfg.model, "JGRIP_F1_R");
        assert_eq!(cfg.twist_mode, Some(0xFF));
        assert_eq!(cfg.twist_label(), "Axis only");
        assert_eq!(cfg.product_name, "UR");
        assert_eq!(cfg.serial, "deadbeef");
        // firmware_complete décodé lisiblement
        let fw = cfg.fields.iter().find(|f| f.name == "firmware_complete").unwrap();
        assert_eq!(fw.human, "Firmware complete");
        // les champs grip_only SONT présents pour une poignée
        assert!(cfg.fields.iter().any(|f| f.name == "z_axis_mode"));
    }

    #[test]
    fn read_controller_skips_grip_only_on_base() {
        let mut t = FakeTransport::open("x").unwrap();
        let cfg = read_controller(&mut t, p::DEVICE_BASE, p::FAMILY_BASE);
        assert_eq!(cfg.model, "J5_BASE");
        // sur la base, les champs grip_only (z_axis_mode, calibration…) sont sautés
        assert!(!cfg.fields.iter().any(|f| f.name == "z_axis_mode"));
        assert!(cfg.fields.iter().any(|f| f.name == "firmware_complete"));
        assert_eq!(cfg.twist_mode, None);
    }

    #[test]
    fn read_device_reads_each_controller() {
        let ctrls = [
            Controller::new(0x0A, p::FAMILY_GRIP),
            Controller::new(p::DEVICE_BASE, p::FAMILY_BASE),
        ];
        let dc = read_device::<FakeTransport>("/dev/hidrawX", &ctrls).unwrap();
        assert_eq!(dc.hidraw, "/dev/hidrawX");
        assert_eq!(dc.controllers.len(), 2);
        assert_eq!(dc.controllers[0].family, p::FAMILY_GRIP);
        assert_eq!(dc.controllers[1].model, "J5_BASE");
    }

    #[test]
    fn read_base_led_decodes() {
        let mut t = FakeTransport::open("x").unwrap();
        t.flash.insert((0x20, p::FAMILY_BASE, p::BASE_BACKLIGHT_OFFSET), [200, 0, 0, 0]);
        t.flash.insert((0x20, p::FAMILY_BASE, p::BASE_BREATHING_OFFSET), [p::BREATHING_VALUE, 0, 0, 0]);
        let led = read_base_led(&mut t, 0x20, p::FAMILY_BASE);
        assert_eq!(led.brightness, Some(200));
        assert_eq!(led.breathing, Some(true)); // 0 = respiration
        // 0xff (non défini) -> rendu 0 ; 0xF8=1 -> fixe
        t.flash.insert((0x20, p::FAMILY_BASE, p::BASE_BACKLIGHT_OFFSET), [0xFF, 0, 0, 0]);
        t.flash.insert((0x20, p::FAMILY_BASE, p::BASE_BREATHING_OFFSET), [p::STATIC_VALUE, 0, 0, 0]);
        let led = read_base_led(&mut t, 0x20, p::FAMILY_BASE);
        assert_eq!(led.brightness, Some(0));
        assert_eq!(led.breathing, Some(false));
    }

    #[test]
    fn read_snapshot_has_base_led() {
        let ctrls = [
            Controller::new(0x0A, p::FAMILY_GRIP),
            Controller::new(p::DEVICE_BASE, p::FAMILY_BASE),
        ];
        let snap = read_snapshot::<FakeTransport>("/dev/hidrawX", &ctrls).unwrap();
        assert_eq!(snap.config.controllers.len(), 2);
        // endpoint avec un contrôleur de base -> état LED présent (valeurs None ici)
        assert!(snap.base_led.is_some());
        // calibration : une entrée par contrôleur
        assert_eq!(snap.calib.len(), 2);
        let grip = snap.calib.iter().find(|c| c.family == p::FAMILY_GRIP).unwrap();
        assert_eq!(grip.axes, ["Z", "Rx", "Ry", "Slider"]);
        assert_eq!(grip.region.len(), 13); // 0xC8..=0xF8 par pas de 4
        let base = snap.calib.iter().find(|c| c.family == p::FAMILY_BASE).unwrap();
        assert_eq!(base.axes, ["X", "Y", "Slider"]);
        assert!(base.region.is_empty()); // pas de région calib affichée pour la base
    }
}

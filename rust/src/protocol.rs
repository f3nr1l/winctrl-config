//! Protocole vendor SimApp Pro des périphériques WinWing — encodé en tables.
//!
//! Source de vérité : `docs/simapppro-protocol.md`. Ce module ne fait *aucune*
//! I/O ; il ne contient que les constantes, l'identité des contrôleurs, la carte
//! des offsets flash et le garde-fou d'identité. La couche `transport` (incrément
//! ultérieur) parle le fil, la couche `model` s'appuie sur ces tables pour
//! décoder/encoder la config.
//!
//! Trame (14 octets) : `[0]=0x02 [1]=device [2]=family [3..4]=0 [5]=len [6]=opcode [7..]=args`.
//! L'écho d'acceptation pose le bit `0x10` sur l'octet family (`0xbf` -> `0xcf`).

use std::fmt;

pub const REPORT_ID: u8 = 0x02;
pub const FRAME_LEN: usize = 14;
/// Posé sur l'octet family dans l'écho d'acceptation.
pub const ECHO_FAMILY_BIT: u8 = 0x10;

/// Une trame vendor complète de longueur fixe.
pub type Frame = [u8; FRAME_LEN];

// --- Opcodes (docs/simapppro-protocol.md §2) ------------------------------
pub const OP_ONLINE_HEARTBEAT: u8 = 0x00;
pub const OP_REQUEST_DEVICE_HW: u8 = 0x01;
pub const OP_REQUEST_DEVICE_FW: u8 = 0x02;
pub const OP_REQUEST_DEVICE_SN: u8 = 0x03;
pub const OP_DEVICE_RESTART: u8 = 0x04;
pub const OP_READ_CFG_DATA: u8 = 0x05;
pub const OP_WRITE_CFG_DATA: u8 = 0x06;
/// 0x00 = appli, 0x01 = bootloader.
pub const OP_REQUEST_DEVICE_MODE: u8 = 0x18;
/// → bootloader : INTERDIT à l'app (sauf module de flash sanctionné [`crate::flash`]).
pub const OP_ENTER_UPDATA_MODE: u8 = 0x40;
// Opcodes de flashage (bootloader) — cf. `docs/firmware-update.md` §2. Bannis du
// chemin d'écriture ordinaire (`FORBIDDEN_OPCODES`) ; seul [`crate::flash`] les émet.
pub const OP_START_UPDATE: u8 = 0x20;
pub const OP_UPDATE_DATA: u8 = 0x21;
pub const OP_UPDATE_DATA_LEN: u8 = 0x22;
pub const OP_UPDATE_DATA_CRC: u8 = 0x23;
pub const OP_QUIT_UPDATA_MODE: u8 = 0x24;
/// Adresse de **diffusion** des ordres bootloader (0x40 / 0x24) : device 0x01, family 0x00.
pub const BROADCAST_DEVICE: u8 = 0x01;
pub const BROADCAST_FAMILY: u8 = 0x00;
/// Démarre la calibration d'un axe (Index).
pub const OP_CALIBRATION_START: u8 = 0x47;
/// Termine ; le FIRMWARE écrit min/centre/max.
pub const OP_CALIBRATION_FINISH: u8 = 0x48;
pub const OP_SET_LEDX: u8 = 0x49;
pub const OP_READ_PARAM_DATA: u8 = 0x55;
pub const OP_WRITE_PARAM_DATA: u8 = 0x56;
pub const OP_SAVE_PARAM_DATA: u8 = 0x57;

/// Opcodes qui écrivent en mémoire non volatile ou changent l'état du device.
/// L'app ne les émet jamais sans passer par la garde write explicite.
pub static WRITE_OPCODES: [u8; 3] = [OP_WRITE_CFG_DATA, OP_WRITE_PARAM_DATA, OP_SAVE_PARAM_DATA];

/// Opcodes formellement bannis de l'app (flashage / bootloader — cf. plan §2).
pub static FORBIDDEN_OPCODES: [u8; 7] =
    [0x20, 0x21, 0x22, 0x23, 0x24, 0x25, OP_ENTER_UPDATA_MODE];

/// True si l'opcode écrit en mémoire non volatile / change l'état du device.
pub fn is_write_opcode(opcode: u8) -> bool {
    WRITE_OPCODES.contains(&opcode)
}

/// True si l'opcode est formellement banni de l'app (flashage / bootloader).
pub fn is_forbidden_opcode(opcode: u8) -> bool {
    FORBIDDEN_OPCODES.contains(&opcode)
}

// --- Identité des contrôleurs (docs/simapppro-protocol.md §3) -------------
/// Poignée : axes, calibration, mapping, moteur.
pub const FAMILY_GRIP: u8 = 0xBF;
/// Base : rétroéclairage, écran.
pub const FAMILY_BASE: u8 = 0xBB;

/// device de la poignée = octet bas du PID USB − 0x20.
/// La base répond toujours sur device 0x20, family 0xbb.
pub const DEVICE_BASE: u8 = 0x20;

/// Noms de modèle par `(device, family)`, pour l'affichage.
pub static CONTROLLER_NAMES: &[((u8, u8), &str)] = &[
    ((0x07, FAMILY_GRIP), "JGRIP_C1_L"), // Civil gauche
    ((0x08, FAMILY_GRIP), "JGRIP_C1_R"), // Civil droite
    ((0x09, FAMILY_GRIP), "JGRIP_F1_L"), // Fighter gauche
    ((0x0A, FAMILY_GRIP), "JGRIP_F1_R"), // Fighter droite
    ((0x0B, FAMILY_GRIP), "JGRIP_S1_L"), // Space gauche
    ((0x0C, FAMILY_GRIP), "JGRIP_S1_R"), // Space droite
    ((0x20, FAMILY_BASE), "J5_BASE"),    // base (commune aux deux mains)
];

/// Nom de modèle d'un `(device, family)`, ou `None` si inconnu.
pub fn controller_name(device: u8, family: u8) -> Option<&'static str> {
    CONTROLLER_NAMES
        .iter()
        .find(|((d, f), _)| *d == device && *f == family)
        .map(|(_, name)| *name)
}

/// Côté déduit du suffixe de modèle, en **source anglaise** (traduit à l'affichage),
/// ou `None`. Utilisé pour le nom commercial ; le choix de la photo se fait à part.
pub fn model_side(model: &str) -> Option<&'static str> {
    if model.ends_with("_L") {
        Some("left")
    } else if model.ends_with("_R") {
        Some("right")
    } else {
        None
    }
}

/// Nom commercial neutre affiché pour un contrôleur.
///
/// Le firmware annonce une famille « Combat/Fighter » (poignée à PID `F1`), mais
/// le variant matériel réel (Space, Combat…) n'est pas déterminable de façon
/// fiable par logiciel. On affiche donc un nom générique de la gamme URSA MINOR ;
/// l'identité technique exacte reste consultable dans « Détails techniques ».
pub fn commercial_name(model: &str) -> String {
    if model.contains("BASE") {
        "URSA MINOR — Base".to_string()
    } else if let Some(side) = model_side(model) {
        format!("URSA MINOR Joystick ({side})")
    } else {
        "URSA MINOR".to_string()
    }
}

/// device de la poignée déduit du PID USB : octet bas − 0x20.
/// Retourne un entier signé : un octet bas < 0x20 donnerait une valeur négative
/// (jamais un device valide), reproduisant le `int` de Python.
pub fn grip_device_from_pid(pid: u16) -> i32 {
    ((pid & 0xFF) as i32) - 0x20
}

/// « pid » de l'API catalogue firmware = `(family << 8) | device`.
pub fn controller_pid(device: u8, family: u8) -> u16 {
    ((family as u16) << 8) | device as u16
}

// --- Mode de l'axe twist (Z/Rz), offset 0xD8 (§6) -------------------------
/// Boutons de position seuls.
pub const Z_MODE_BUTTONS_ONLY: u8 = 0x00;
/// Axe analogique ET boutons (double-mappé).
pub const Z_MODE_AXIS_AND_BUTTONS: u8 = 0x01;
/// Axe analogique seul (propre).
pub const Z_MODE_AXIS_ONLY: u8 = 0xFF;

/// Libellés des modes twist (valeur brute -> texte source anglais). Le cœur reste
/// sans dépendance i18n : la traduction (gettext) est appliquée côté UI.
pub static Z_MODE_LABELS: &[(u8, &str)] = &[
    (Z_MODE_BUTTONS_ONLY, "Buttons only"),
    (Z_MODE_AXIS_AND_BUTTONS, "Axis and buttons"),
    (Z_MODE_AXIS_ONLY, "Axis only"),
];

/// Libellé d'un mode twist, ou `None` si valeur inconnue.
pub fn z_mode_label(mode: u8) -> Option<&'static str> {
    Z_MODE_LABELS
        .iter()
        .find(|(v, _)| *v == mode)
        .map(|(_, l)| *l)
}

// --- Restore-default (offset 0xB4, §7) ------------------------------------
// « Restaurer la configuration par défaut » : écrire ce drapeau puis
// DEVICE_RESTART. Le firmware relit le drapeau au reboot et reconstruit sa
// config d'usine.
pub const RESTORE_DEFAULT_OFFSET: u32 = 0xB4;
pub const RESTORE_DEFAULT_DATA: [u8; 4] = [1, 0, 0, 0];

// --- Offsets à sémantique par-device --------------------------------------
// Un même offset flash a une sémantique DIFFÉRENTE selon le device visé (base
// 0xbb vs poignée 0xbf). Les writes ci-dessous sont donc toujours adressés
// (offset + family). Ne jamais présumer le sens d'un offset hors de sa family.
/// SET_LEDX index 0 : rétroéclairage (base) / moteur (grip).
pub const LED_INDEX_BACKLIGHT: u8 = 0;

// Base (family 0xbb) :
/// octet[0] = 0/1 (mode 4x32), + restart requis.
pub const BASE_4X32_OFFSET: u32 = 0xC8;
/// uint32 LE — zone morte axe X.
pub const BASE_DEADZONE_X_OFFSET: u32 = 0xD8;
/// uint32 LE — zone morte axe Y.
pub const BASE_DEADZONE_Y_OFFSET: u32 = 0xE8;
/// octet[0] = luminosité persistée (0..255).
pub const BASE_BACKLIGHT_OFFSET: u32 = 0xEC;
/// octet[0] = état de la lumière (voir `BREATHING_VALUE`).
pub const BASE_BREATHING_OFFSET: u32 = 0xF8;

// Sémantique de 0xF8[0], vérifiée sur matériel : la valeur 0 fait respirer la
// lumière, la valeur 1 la rend fixe.
/// respiration.
pub const BREATHING_VALUE: u8 = 0x00;
/// fixe.
pub const STATIC_VALUE: u8 = 0x01;

// Poignée (family 0xbf) :
/// `[shape(0=carré/255=rond), map_mode(0/1/255), b2, b3]`.
pub const GRIP_MINISTICK_OFFSET: u32 = 0xFC;
/// octet[2] = zone morte twist Rz (0..30).
pub const GRIP_DEADZONE_TWIST_OFFSET: u32 = 0x104;

// Mode du mini-stick Rx/Ry : octet[1] de 0xFC, MÊME sémantique 3 états que le
// twist Z (0xD8). On réutilise Z_MODE_* / z_mode_label pour le décoder.
pub const MINISTICK_SHAPE_SQUARE: u8 = 0x00;
pub const MINISTICK_SHAPE_ROUND: u8 = 0xFF;

/// Libellés de la forme du mini-stick (valeur brute -> texte).
pub static MINISTICK_SHAPE_LABELS: &[(u8, &str)] =
    &[(MINISTICK_SHAPE_SQUARE, "carré"), (MINISTICK_SHAPE_ROUND, "rond")];

/// Libellé de la forme du mini-stick, ou `None` si valeur inconnue.
pub fn ministick_shape_label(shape: u8) -> Option<&'static str> {
    MINISTICK_SHAPE_LABELS
        .iter()
        .find(|(v, _)| *v == shape)
        .map(|(_, l)| *l)
}

/// Deadzone twist : valeur max autorisée de l'octet 0x104[2].
pub const DEADZONE_TWIST_MAX: u8 = 30;

// --- Calibration (D-3, opcodes 0x47/0x48) ---------------------------------
// Framing CONFIRMÉ sur matériel (probe 2026-08-28) :
//   start  : 02 <dev> <fam> 00 00 02 47 <index>
//   finish : 02 <dev> <fam> 00 00 02 48 <index>   (écho <fam|0x10> … 47/48 <index> 01)
// Séquence : start(index) → l'utilisateur balaie l'axe puis recentre → finish(index)
// → le FIRMWARE écrit min/centre/max dans la région flash 0xC8–0xF8 (l'hôte ne les
// écrit pas). Toujours sauvegarder toute la région avant (plusieurs mots touchés).
pub const CALIB_REGION_START: u32 = 0xC8;
pub const CALIB_REGION_END: u32 = 0xF8;

/// Index d'axe canonique (Global.js IndexToAxisName).
pub static AXIS_INDEX_NAMES: &[(u8, &str)] = &[
    (0, "X"),
    (1, "Y"),
    (2, "Z"),
    (3, "Rx"),
    (4, "Ry"),
    (5, "Rz"),
    (6, "Slider"),
    (7, "Dial"),
];

/// Nom canonique d'un index d'axe, ou `None`.
pub fn axis_index_name(index: u8) -> Option<&'static str> {
    AXIS_INDEX_NAMES
        .iter()
        .find(|(i, _)| *i == index)
        .map(|(_, n)| *n)
}

/// Index d'un axe à partir de son nom canonique, ou `None`.
pub fn axis_name_index(name: &str) -> Option<u8> {
    AXIS_INDEX_NAMES
        .iter()
        .find(|(_, n)| *n == name)
        .map(|(i, _)| *i)
}

// Axes calibrables par contrôleur (DeviceConfig.js) : base = X,Y,Slider ;
// poignée = Z,Rx,Ry,Slider. Le slider physique de l'URSA est sur la base.
pub static CALIB_AXES_BASE: [u8; 3] = [0, 1, 6];
pub static CALIB_AXES_GRIP: [u8; 4] = [2, 3, 4, 6];

/// Index d'axes calibrables pour une famille de contrôleur.
pub fn calibration_indexes(family: u8) -> &'static [u8] {
    match family {
        FAMILY_GRIP => &CALIB_AXES_GRIP,
        FAMILY_BASE => &CALIB_AXES_BASE,
        _ => &[],
    }
}

/// Offsets (pas de 4 o) de la région de calibration 0xC8–0xF8 incluse.
pub fn calib_region_offsets() -> Vec<u32> {
    (CALIB_REGION_START..=CALIB_REGION_END).step_by(4).collect()
}

// --- Offsets d'écriture INTERDITS (RE §5, sécurité) -----------------------
// Sémantique non comprise / proche identité → jamais écrire, quel que soit le
// device. `0x100` = switchEquipment (flag variant, proche identité) ; `0xF0` =
// axiosXYMode (overloadé, diffère L↔R, jamais écrit par la grip F1/S1 URSA).
pub static FORBIDDEN_WRITE_OFFSETS: [u32; 2] = [0xF0, 0x100];

/// True si un bloc de 4 o à `offset` chevauche un offset d'écriture interdit.
pub fn is_forbidden_write_offset(offset: u32) -> bool {
    let (lo, hi) = (offset, offset + 3);
    FORBIDDEN_WRITE_OFFSETS.iter().any(|&o| lo <= o + 3 && hi >= o)
}

// --- Carte des offsets flash (poignée 0xbf, §4) ---------------------------
/// Un champ nommé de la config flash on-board.
///
/// `grip_only` : le sens de l'offset n'est établi que pour la poignée (family
/// 0xbf). Sur la base (0xbb), les mêmes offsets recouvrent d'autres champs
/// (rétroéclairage/écran) — on ne les décode donc pas comme des champs de
/// poignée (cf. docs §4, note « le sens d'un offset dépend du device visé »).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashField {
    pub offset: u32,
    /// Nom technique — jamais traduit (cf. i18n §3.1).
    pub name: &'static str,
    /// Description lisible — traduisible côté UI.
    pub note: &'static str,
    /// True = offset d'identité, JAMAIS écrire (§5).
    pub identity: bool,
    /// Sémantique valable seulement pour la poignée 0xbf.
    pub grip_only: bool,
}

impl FlashField {
    const fn new(offset: u32, name: &'static str, note: &'static str) -> Self {
        FlashField { offset, name, note, identity: false, grip_only: false }
    }
    const fn identity(offset: u32, name: &'static str, note: &'static str) -> Self {
        FlashField { offset, name, note, identity: true, grip_only: false }
    }
    const fn grip(offset: u32, name: &'static str, note: &'static str) -> Self {
        FlashField { offset, name, note, identity: false, grip_only: true }
    }
}

/// Ordre = ordre de lecture pour l'affichage. `identity=true` marque les plages
/// protégées : le garde-fou write les refuse en dur.
pub static FLASH_FIELDS: &[FlashField] = &[
    FlashField::new(0x04, "firmware_complete", "drapeau d'écriture firmware achevée (=1)"),
    FlashField::identity(0x08, "type_id", "type/ID du modèle"),
    FlashField::identity(0x14, "device_family", "device + family du contrôleur"),
    FlashField::identity(0x5C, "product_name", "nom produit ASCII (0x5c–0x84)"),
    FlashField::identity(0x9C, "serial", "numéro de série (0x9c–0xa4)"),
    FlashField::new(0xB0, "hardware_version", "version matérielle (recoupe REQUEST_DEVICE_HW)"),
    FlashField::new(0xB4, "restore_default", "drapeau « restaurer config par défaut »"),
    FlashField::grip(0xC0, "cfg_c0", "config (diffère L↔R)"),
    FlashField::grip(0xC8, "calibration", "calibration des axes min/centre/max (0xc8–0xf8)"),
    FlashField::grip(0xD8, "z_axis_mode", "mode de l'axe twist Z/Rz [mode,0xff,spd_lo,spd_hi]"),
    FlashField::grip(0xF0, "axis_xy_mode", "mode de sortie XY (axiosXYMode)"),
];

// Plages d'octets d'identité — INTERDICTION d'écriture (docs §5, plan §5).
// (début, fin_incluse) en octets.
pub static IDENTITY_RANGES: [(u32, u32); 4] = [
    (0x08, 0x0B), // type/ID
    (0x14, 0x17), // device + family
    (0x5C, 0x84), // nom produit
    (0x9C, 0xA4), // numéro de série
];

/// True si l'offset (bloc de 4 o) chevauche une plage d'identité protégée.
pub fn is_identity_offset(offset: u32) -> bool {
    let (lo, hi) = (offset, offset + 3);
    IDENTITY_RANGES.iter().any(|&(r_lo, r_hi)| lo <= r_hi && hi >= r_lo)
}

/// Offsets présents dans la config mais EXCLUS d'un profil : drapeaux et
/// compteurs qui ne sont pas de la « config à transférer » d'un manche à
/// l'autre (les réécrire n'a pas de sens, voire est nuisible).
pub static PROFILE_SKIP_OFFSETS: [u32; 3] = [
    0x04, // firmware_complete — drapeau
    0xB4, // restore_default   — drapeau de commande
    0xC0, // compteur d'écritures
];

/// True si l'offset peut entrer dans un profil (config transférable) :
/// ni identité protégée, ni drapeau/compteur, ni offset d'écriture interdit
/// (`0xF0`/`0x100`). Liste blanche par exclusion, portée dans la couche
/// Protocole (garde-fou n°1, cf. architecture §2.4).
pub fn is_profile_writable(offset: u32) -> bool {
    !is_identity_offset(offset)
        && !is_forbidden_write_offset(offset)
        && !PROFILE_SKIP_OFFSETS.contains(&offset)
}

// --- Garde-fou d'écriture (§2.4 / §6.1) -----------------------------------
/// Refus d'écriture porté dans la couche Protocole, réutilisé tel quel par le
/// futur `write_cfg` du Transport. Reproduit la garde de
/// `transport.py::write_cfg` : refuse en dur les offsets d'identité (sauf
/// `allow_identity`, que l'app ne pose JAMAIS) et toujours les offsets interdits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteGuardError {
    /// Offset dans une plage d'identité protégée (nom/PID/série/type).
    Identity(u32),
    /// Offset formellement interdit (sémantique non comprise, cf. RE §5).
    Forbidden(u32),
}

impl fmt::Display for WriteGuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            WriteGuardError::Identity(off) => write!(
                f,
                "écriture refusée : offset {off:#04x} dans une plage d'identité"
            ),
            WriteGuardError::Forbidden(off) => write!(
                f,
                "écriture refusée : offset {off:#04x} formellement interdit \
                 (sémantique non comprise, cf. RE §5)"
            ),
        }
    }
}

impl std::error::Error for WriteGuardError {}

/// Applique le garde-fou d'écriture pour `offset`. `allow_identity` n'est vrai
/// que pour un outil de réparation hors-app — l'app ne le pose JAMAIS.
pub fn guard_write(offset: u32, allow_identity: bool) -> Result<(), WriteGuardError> {
    if !allow_identity && is_identity_offset(offset) {
        return Err(WriteGuardError::Identity(offset));
    }
    if is_forbidden_write_offset(offset) {
        return Err(WriteGuardError::Forbidden(offset));
    }
    Ok(())
}

// --- Construction de trame ------------------------------------------------
/// Construit la trame vendor de 14 octets. `len` (octet [5]) compte l'opcode.
///
/// `args` ne doit pas dépasser 7 octets (la plus grande charge utile — write
/// cfg : 3 o d'offset + 4 o de données — remplit exactement la trame).
pub fn build_frame(device: u8, family: u8, opcode: u8, args: &[u8]) -> Frame {
    let mut b = [0u8; FRAME_LEN];
    b[0] = REPORT_ID;
    b[1] = device;
    b[2] = family;
    b[5] = (1 + args.len()) as u8;
    b[6] = opcode;
    b[7..7 + args.len()].copy_from_slice(args);
    b
}

/// Offset flash sur 3 octets petit-boutiens.
pub fn offset_bytes(offset: u32) -> [u8; 3] {
    [
        (offset & 0xFF) as u8,
        ((offset >> 8) & 0xFF) as u8,
        ((offset >> 16) & 0xFF) as u8,
    ]
}

/// Rendu hexa d'un buffer : octets minuscules séparés par une espace.
pub fn hx(data: &[u8]) -> String {
    data.iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Garde-fou d'identité (mirroir de test_protocol.py) ---------------
    #[test]
    fn identity_ranges_flagged() {
        // tous les offsets des plages d'identité doivent être protégés
        for &(lo, hi) in IDENTITY_RANGES.iter() {
            let mut off = lo & !3;
            while off <= hi {
                assert!(is_identity_offset(off), "{off:#04x} devrait être identité");
                off += 4;
            }
        }
    }

    #[test]
    fn non_identity_offsets_writable() {
        for off in [0x04, 0xB0, 0xB4, 0xC0, 0xC8, 0xD8, 0xF0, 0x100] {
            assert!(
                !is_identity_offset(off),
                "{off:#04x} ne devrait pas être identité"
            );
        }
    }

    #[test]
    fn overlap_partial_block() {
        // un bloc de 4 o qui chevauche même partiellement une plage est protégé
        assert!(is_identity_offset(0x84)); // fin du nom produit
        assert!(is_identity_offset(0xA4)); // fin du serial
    }

    #[test]
    fn boundaries_clear() {
        // juste au-delà des plages : écrivable
        assert!(!is_identity_offset(0x88)); // après le nom (0x84+4)
        assert!(!is_identity_offset(0x18)); // après device_family
    }

    // --- Construction de trame --------------------------------------------
    #[test]
    fn offset_bytes_le() {
        assert_eq!(offset_bytes(0xD8), [0xD8, 0x00, 0x00]);
        assert_eq!(offset_bytes(0x012345), [0x45, 0x23, 0x01]);
    }

    #[test]
    fn build_frame_read_cfg() {
        // trame READ_CFG_DATA de la doc §4 : 02 0a bf 00 00 04 05 d8 00 00
        let f = build_frame(0x0A, FAMILY_GRIP, OP_READ_CFG_DATA, &offset_bytes(0xD8));
        assert_eq!(f.len(), FRAME_LEN);
        assert_eq!(
            &f[..10],
            &[0x02, 0x0A, 0xBF, 0, 0, 0x04, 0x05, 0xD8, 0, 0]
        );
    }

    #[test]
    fn build_frame_len_counts_opcode() {
        let f = build_frame(0x20, FAMILY_BASE, OP_DEVICE_RESTART, &[]);
        assert_eq!(f[5], 1); // len = opcode seul
        assert_eq!(f[6], OP_DEVICE_RESTART);
    }

    #[test]
    fn build_frame_set_led() {
        // 02 20 bb 00 00 03 49 00 c8  (base, index 0, valeur 200)
        let f = build_frame(DEVICE_BASE, FAMILY_BASE, OP_SET_LEDX, &[0, 200]);
        assert_eq!(&hx(&f)[..26], "02 20 bb 00 00 03 49 00 c8");
    }

    #[test]
    fn build_frame_4x32_write() {
        // 02 20 bb 00 00 08 06 c8 00 00 01 00 00 00
        let mut args = offset_bytes(BASE_4X32_OFFSET).to_vec();
        args.extend_from_slice(&[1, 0, 0, 0]);
        let f = build_frame(DEVICE_BASE, FAMILY_BASE, OP_WRITE_CFG_DATA, &args);
        assert_eq!(hx(&f), "02 20 bb 00 00 08 06 c8 00 00 01 00 00 00");
    }

    // --- Dérivation d'identité --------------------------------------------
    #[test]
    fn grip_device_from_pid_low_byte() {
        assert_eq!(grip_device_from_pid(0xBC2A), 0x0A); // R
        assert_eq!(grip_device_from_pid(0xBC29), 0x09); // L
    }

    #[test]
    fn controller_pid_api() {
        assert_eq!(controller_pid(0x0A, FAMILY_GRIP), 0xBF0A);
        assert_eq!(controller_pid(0x20, FAMILY_BASE), 0xBB20);
    }

    #[test]
    fn controller_name_lookup() {
        assert_eq!(controller_name(0x0A, FAMILY_GRIP), Some("JGRIP_F1_R"));
        assert_eq!(controller_name(0x20, FAMILY_BASE), Some("J5_BASE"));
        assert_eq!(controller_name(0x00, FAMILY_GRIP), None);
    }

    #[test]
    fn commercial_name_is_neutral() {
        // Nom commercial générique : ne prétend jamais « Space » ni « Fighter »
        // (variant indéterminable par logiciel), seulement le côté et la gamme.
        assert_eq!(commercial_name("JGRIP_F1_R"), "URSA MINOR Joystick (right)");
        assert_eq!(commercial_name("JGRIP_S1_L"), "URSA MINOR Joystick (left)");
        assert_eq!(commercial_name("J5_BASE"), "URSA MINOR — Base");
        assert_eq!(commercial_name("?"), "URSA MINOR");
        assert_eq!(model_side("JGRIP_F1_R"), Some("right"));
        assert_eq!(model_side("J5_BASE"), None);
    }

    // --- Offsets interdits (mirroir de test_writes.py) --------------------
    #[test]
    fn forbidden_write_predicate() {
        assert!(is_forbidden_write_offset(0xF0));
        assert!(is_forbidden_write_offset(0x100));
        assert!(!is_forbidden_write_offset(0xEC));
        assert!(!is_forbidden_write_offset(0xFC));
    }

    #[test]
    fn guard_refuses_forbidden_and_identity() {
        // offsets interdits : refusés même avec allow_identity
        assert_eq!(guard_write(0xF0, false), Err(WriteGuardError::Forbidden(0xF0)));
        assert_eq!(guard_write(0x100, false), Err(WriteGuardError::Forbidden(0x100)));
        assert_eq!(guard_write(0xF0, true), Err(WriteGuardError::Forbidden(0xF0)));
        // offsets d'identité : refusés sauf allow_identity (jamais posé par l'app)
        assert_eq!(guard_write(0x08, false), Err(WriteGuardError::Identity(0x08)));
        assert_eq!(guard_write(0x5C, false), Err(WriteGuardError::Identity(0x5C)));
        assert_eq!(guard_write(0x08, true), Ok(()));
        // offsets de config ordinaires : autorisés
        assert_eq!(guard_write(0xD8, false), Ok(()));
        assert_eq!(guard_write(0xEC, false), Ok(()));
    }

    // --- Profil écrivable -------------------------------------------------
    #[test]
    fn profile_writable_excludes_the_right_offsets() {
        // exclus : identité, interdits (0xF0/0x100), drapeaux/compteur
        for off in [0x08, 0x14, 0x5C, 0x9C, 0xF0, 0x100, 0x04, 0xB4, 0xC0] {
            assert!(!is_profile_writable(off), "{off:#04x} ne devrait pas être profilable");
        }
        // transférables : config utile
        for off in [0xB0, 0xC8, 0xD8, 0xE8, 0xEC, 0xF8, 0xFC, 0x104] {
            assert!(is_profile_writable(off), "{off:#04x} devrait être profilable");
        }
    }

    // --- Calibration ------------------------------------------------------
    #[test]
    fn calibration_indexes_per_family() {
        assert_eq!(calibration_indexes(FAMILY_GRIP), &[2, 3, 4, 6]);
        assert_eq!(calibration_indexes(FAMILY_BASE), &[0, 1, 6]);
        assert_eq!(calibration_indexes(0x00), &[] as &[u8]);
    }

    #[test]
    fn calib_region_offsets_step4() {
        let offs = calib_region_offsets();
        assert_eq!(offs.first(), Some(&0xC8));
        assert_eq!(offs.last(), Some(&0xF8));
        // 0xC8..=0xF8 par pas de 4 = 13 mots
        assert_eq!(offs.len(), 13);
        assert!(offs.iter().all(|o| (o - CALIB_REGION_START) % 4 == 0));
    }

    #[test]
    fn axis_name_roundtrip() {
        assert_eq!(axis_index_name(2), Some("Z"));
        assert_eq!(axis_index_name(6), Some("Slider"));
        assert_eq!(axis_name_index("Rx"), Some(3));
        assert_eq!(axis_index_name(99), None);
    }

    #[test]
    fn opcode_classifiers() {
        assert!(is_write_opcode(OP_WRITE_CFG_DATA));
        assert!(!is_write_opcode(OP_READ_CFG_DATA));
        assert!(is_forbidden_opcode(OP_ENTER_UPDATA_MODE));
        assert!(is_forbidden_opcode(0x22));
        assert!(!is_forbidden_opcode(OP_WRITE_CFG_DATA));
    }
}

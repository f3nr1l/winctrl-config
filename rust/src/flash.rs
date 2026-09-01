//! Flashage du firmware (bootloader) — opération irréversible.
//!
//! Ces opcodes (`0x40/0x20/0x21/0x22/0x23/0x24`) font passer le contrôleur en
//! bootloader et réécrivent sa mémoire non volatile. Une erreur peut rendre le
//! manche inutilisable. Ils sont bannis du chemin d'écriture ordinaire
//! ([`crate::protocol::FORBIDDEN_OPCODES`]) ; ce module est la seule exception,
//! à n'utiliser qu'avec un firmware officiel et sur décision explicite.
//!
//! Ce fichier contient le cœur pur (parsing/CRC/validation/génération de trames,
//! testable) et le pilote ([`run_flash`]) qui émet la séquence et gère la
//! ré-énumération USB (bootloader). Le pilote fait un dry-run par défaut.

use std::time::Duration;

use crate::enumerate::WinwingDevice;
use crate::model::Transport;
use crate::protocol as p;
use crate::transport::HidrawTransport;

// --- CRC-16/MODBUS (établi, cf. firmware-update.md §2.6) --------------------
/// CRC-16/MODBUS : poly `0x8005` réfléchi (`0xA001`), init `0xFFFF`, refin/refout,
/// xorout `0`. Porte sur la **charge utile transmise**.
pub fn crc16_modbus(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

// --- Format .wwtc (reverse/firmware-header-RE.md §3) -----------------------
/// En-tête `.wwtc` (8 octets) : identité du contrôleur cible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WwtcHeader {
    pub hw_type: u16,
    pub device: u8,
    pub family: u8,
    pub hardware_version: u16,
    pub fw_minor: u8,
    pub fw_major: u8,
}

impl WwtcHeader {
    /// Version lisible « major.minor » (minor en **hexa** : throttle `0x16` = « 1.16 »),
    /// même format que le nom de fichier, le catalogue et `REQUEST_DEVICE_FW`.
    pub fn version(&self) -> String {
        format!("{}.{:02x}", self.fw_major, self.fw_minor)
    }
}

/// Firmware parsé : en-tête + charge utile (à transmettre) + longueur/CRC déclarés.
#[derive(Debug, Clone)]
pub struct Firmware {
    pub header: WwtcHeader,
    pub payload: Vec<u8>,
    pub declared_len: u32,
    pub declared_crc: u16,
}

impl Firmware {
    /// Nombre de trames `UPDATE_DATA` (4 octets chacune).
    pub fn n_data_frames(&self) -> usize {
        self.payload.len() / 4
    }
}

/// Parse et **valide** un `.wwtc` : en-tête, payload, pied (longueur + CRC).
/// Vérifie `declared_len == payload.len()`, `payload.len() % 4 == 0`, et
/// `declared_crc == crc16_modbus(payload)`. Erreur explicite sinon.
pub fn parse_wwtc(bytes: &[u8]) -> Result<Firmware, String> {
    if bytes.len() < 14 {
        return Err(format!("fichier trop court ({} o < 14)", bytes.len()));
    }
    let n = bytes.len();
    let header = WwtcHeader {
        hw_type: u16::from_le_bytes([bytes[0], bytes[1]]),
        device: bytes[2],
        family: bytes[3],
        hardware_version: u16::from_le_bytes([bytes[4], bytes[5]]),
        fw_minor: bytes[6],
        fw_major: bytes[7],
    };
    let payload = bytes[8..n - 6].to_vec();
    let declared_len = u32::from_le_bytes([bytes[n - 6], bytes[n - 5], bytes[n - 4], bytes[n - 3]]);
    let declared_crc = u16::from_le_bytes([bytes[n - 2], bytes[n - 1]]);

    if declared_len as usize != payload.len() {
        return Err(format!(
            "longueur du pied ({declared_len}) ≠ payload ({})",
            payload.len()
        ));
    }
    if payload.len() % 4 != 0 {
        return Err(format!(
            "payload ({} o) non multiple de 4 (trames de 4 octets)",
            payload.len()
        ));
    }
    let crc = crc16_modbus(&payload);
    if crc != declared_crc {
        return Err(format!(
            "CRC-16/MODBUS incorrect : calculé {crc:#06x}, déclaré {declared_crc:#06x} — fichier corrompu"
        ));
    }
    Ok(Firmware {
        header,
        payload,
        declared_len,
        declared_crc,
    })
}

/// Contrôleur cible d'un flash : identité + version matérielle (pour les 4 contrôles).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlashTarget {
    pub device: u8,
    pub family: u8,
    pub hwver: u16,
    pub hw_type: u16,
}

/// Vérifie que le firmware **cible bien** ce contrôleur (4 contrôles de la DLL :
/// hardware type, device, family, hardware version). Erreur nommée si non.
pub fn check_target(fw: &Firmware, t: &FlashTarget) -> Result<(), String> {
    let h = &fw.header;
    if h.device != t.device || h.family != t.family {
        return Err(format!(
            "firmware pour {:#04x}/{:#04x}, contrôleur {:#04x}/{:#04x}",
            h.device, h.family, t.device, t.family
        ));
    }
    if h.hardware_version != t.hwver {
        return Err(format!(
            "version matérielle : firmware {:#06x}, contrôleur {:#06x}",
            h.hardware_version, t.hwver
        ));
    }
    if h.hw_type != t.hw_type {
        return Err(format!(
            "type matériel : firmware {:#06x}, contrôleur {:#06x}",
            h.hw_type, t.hw_type
        ));
    }
    Ok(())
}

/// Arguments d'une trame `UPDATE_DATA 0x21` : offset 3 octets LE + 4 octets de data.
pub fn update_data_args(offset: u32, chunk: [u8; 4]) -> [u8; 7] {
    let o = p::offset_bytes(offset);
    [o[0], o[1], o[2], chunk[0], chunk[1], chunk[2], chunk[3]]
}

/// Itère les `(offset, chunk4)` de la charge utile (offsets 0, 4, 8, … contigus).
pub fn data_chunks(payload: &[u8]) -> impl Iterator<Item = (u32, [u8; 4])> + '_ {
    payload
        .chunks_exact(4)
        .enumerate()
        .map(|(i, c)| (i as u32 * 4, [c[0], c[1], c[2], c[3]]))
}

// === Pilote (impur : émet des trames, gère la ré-énumération USB) ===========

/// Options de flashage. `dry_run = true` (défaut) **n'émet rien** : valide et décrit.
pub struct FlashOptions {
    pub dry_run: bool,
}

impl Default for FlashOptions {
    fn default() -> Self {
        FlashOptions { dry_run: true }
    }
}

/// Étapes rapportées à l'appelant (pour journal / barre de progression).
#[derive(Debug, Clone)]
pub enum FlashProgress {
    Info(String),
    /// Écriture en cours : `(octets écrits, total)`.
    Writing(usize, usize),
}

/// Délais de ré-énumération (cf. firmware-update.md : ~4 s entrée, ~3 s sortie).
const REENUM_TIMEOUT: Duration = Duration::from_secs(12);
const REENUM_POLL: Duration = Duration::from_millis(400);
const FLASH_ECHO_TIMEOUT: Duration = Duration::from_millis(1500);
const ERASE_TIMEOUT: Duration = Duration::from_secs(5);

/// Exécute (ou simule, si `dry_run`) le flashage d'un contrôleur.
///
/// `dev` désigne le manche ; `device`/`family` le contrôleur ciblé ;
/// `hw_type`/`hwver` sa version matérielle (pour les 4 contrôles). `progress` reçoit
/// les étapes. **N'émet la moindre trame que si `!opts.dry_run`.**
///
/// Le chemin d'émission réel n'est pas couvert par les tests automatisés (qui
/// valident le cœur pur). Le dry-run par défaut protège contre un flash accidentel.
pub fn run_flash(
    dev: &WinwingDevice,
    target: &FlashTarget,
    fw: &Firmware,
    opts: &FlashOptions,
    mut progress: impl FnMut(FlashProgress),
) -> Result<(), String> {
    let (device, family) = (target.device, target.family);
    check_target(fw, target)?;
    let total = fw.payload.len();
    progress(FlashProgress::Info(format!(
        "Firmware {} — {} octets, {} trames, CRC {:#06x}",
        fw.header.version(),
        total,
        fw.n_data_frames(),
        fw.declared_crc
    )));

    if opts.dry_run {
        progress(FlashProgress::Info(
            "DRY-RUN : aucune trame émise. Plan validé — décochez le mode simulation pour flasher.".into(),
        ));
        return Ok(());
    }

    // --- 1. Entrée en bootloader (diffusion) ---------------------------------
    progress(FlashProgress::Info("Passage en bootloader (0x40)…".into()));
    {
        let t = open(&dev.hidraw)?;
        let frame = p::build_frame(p::BROADCAST_DEVICE, p::BROADCAST_FAMILY, p::OP_ENTER_UPDATA_MODE, &[]);
        t.flash_exchange(&frame, p::OP_ENTER_UPDATA_MODE, FLASH_ECHO_TIMEOUT)
            .ok_or("pas d'accusé à ENTER_UPDATA_MODE")?;
    } // fermé : le device va disparaître de l'USB

    // --- 2. Ré-énumération + vérification du mode bootloader ------------------
    progress(FlashProgress::Info("Attente de la ré-énumération (bootloader)…".into()));
    let boot_path = wait_for_mode(device, family, 0x01)?;
    progress(FlashProgress::Info(format!("Bootloader confirmé sur {boot_path}")));

    // --- 3. Démarrage (effacement) -------------------------------------------
    let t = open(&boot_path)?;
    progress(FlashProgress::Info("Effacement (START_UPDATE 0x20)…".into()));
    let ack = t
        .flash_exchange(&p::build_frame(device, family, p::OP_START_UPDATE, &[]), p::OP_START_UPDATE, ERASE_TIMEOUT)
        .ok_or("pas d'accusé à START_UPDATE")?;
    if ack.first() != Some(&0x01) {
        return Err(format!("START_UPDATE : statut inattendu {ack:02x?}"));
    }

    // --- 4. Corps du firmware (UPDATE_DATA 0x21) -----------------------------
    progress(FlashProgress::Info("Écriture du firmware…".into()));
    for (offset, chunk) in data_chunks(&fw.payload) {
        let args = update_data_args(offset, chunk);
        let ack = t
            .flash_exchange(&p::build_frame(device, family, p::OP_UPDATE_DATA, &args), p::OP_UPDATE_DATA, FLASH_ECHO_TIMEOUT)
            .ok_or_else(|| format!("pas d'accusé à l'offset {offset}"))?;
        // L'accusé rejoue l'offset 3 octets LE.
        let expect = p::offset_bytes(offset);
        if ack.get(..3) != Some(&expect[..]) {
            return Err(format!("offset {offset} : accusé incohérent {ack:02x?}"));
        }
        if offset as usize % 4096 == 0 {
            progress(FlashProgress::Writing(offset as usize, total));
        }
    }
    progress(FlashProgress::Writing(total, total));

    // --- 5. Clôture : longueur puis CRC --------------------------------------
    progress(FlashProgress::Info("Longueur + CRC…".into()));
    let len_args = p::offset_bytes(fw.declared_len); // 3 octets LE
    t.flash_exchange(&p::build_frame(device, family, p::OP_UPDATE_DATA_LEN, &len_args), p::OP_UPDATE_DATA_LEN, FLASH_ECHO_TIMEOUT)
        .ok_or("pas d'accusé à UPDATE_DATA_LEN")?;
    let crc_args = fw.declared_crc.to_le_bytes();
    t.flash_exchange(&p::build_frame(device, family, p::OP_UPDATE_DATA_CRC, &crc_args), p::OP_UPDATE_DATA_CRC, ERASE_TIMEOUT)
        .ok_or("pas d'accusé à UPDATE_DATA_CRC (CRC refusé ?)")?;

    // --- 6. Sortie du bootloader (diffusion) ---------------------------------
    progress(FlashProgress::Info("Sortie du bootloader (0x24)…".into()));
    let quit = t
        .flash_exchange(&p::build_frame(p::BROADCAST_DEVICE, p::BROADCAST_FAMILY, p::OP_QUIT_UPDATA_MODE, &[]), p::OP_QUIT_UPDATA_MODE, FLASH_ECHO_TIMEOUT)
        .ok_or("pas d'accusé à QUIT_UPDATA_MODE")?;
    if quit.first() != Some(&0x01) {
        return Err(format!("QUIT_UPDATA_MODE : statut inattendu {quit:02x?}"));
    }
    drop(t);

    // --- 7. Ré-énumération en mode applicatif --------------------------------
    progress(FlashProgress::Info("Attente du retour en mode applicatif…".into()));
    let app_path = wait_for_mode(device, family, 0x00)?;
    progress(FlashProgress::Info(format!(
        "Flash terminé — contrôleur de retour en mode applicatif ({app_path})."
    )));
    Ok(())
}

/// Ouvre un nœud hidraw pour le flash.
fn open(hidraw: &str) -> Result<HidrawTransport, String> {
    HidrawTransport::open(hidraw).map_err(|e| format!("ouverture {hidraw} : {e}"))
}

/// Attend qu'un contrôleur `(device, family)` réponde `REQUEST_DEVICE_MODE == mode`
/// (0x01 bootloader / 0x00 applicatif). Balaie **tous** les `/dev/hidraw*` : en
/// bootloader le manche réapparaît sous un autre PID que `enumerate` peut filtrer.
/// Rend le chemin du nœud qui répond. Envoyer une trame vendor à un hidraw étranger
/// est sans effet (pas d'écho).
fn wait_for_mode(device: u8, family: u8, mode: u8) -> Result<String, String> {
    let deadline = std::time::Instant::now() + REENUM_TIMEOUT;
    loop {
        for cand in all_hidraw_paths() {
            if let Ok(mut t) = HidrawTransport::open(&cand) {
                if let Some(pl) = t.request(device, family, p::OP_REQUEST_DEVICE_MODE) {
                    // payload (opcode retiré) : [mode] ou [dev, fam, mode]… -> dernier octet.
                    if pl.last() == Some(&mode) {
                        return Ok(cand);
                    }
                }
            }
        }
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "délai dépassé : aucun contrôleur {device:#04x}/{family:#04x} en mode {mode:#04x}"
            ));
        }
        std::thread::sleep(REENUM_POLL);
    }
}

/// Tous les nœuds `/dev/hidraw*` présents, triés.
fn all_hidraw_paths() -> Vec<String> {
    let mut v = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/dev") {
        for e in rd.flatten() {
            let name = e.file_name();
            let s = name.to_string_lossy();
            if s.starts_with("hidraw") {
                v.push(format!("/dev/{s}"));
            }
        }
    }
    v.sort();
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc16_modbus_known_vector() {
        // Vecteur de référence : "123456789" -> 0x4B37 (CRC-16/MODBUS).
        assert_eq!(crc16_modbus(b"123456789"), 0x4B37);
        assert_eq!(crc16_modbus(&[]), 0xFFFF);
    }

    /// Construit un `.wwtc` synthétique valide autour d'un payload donné.
    fn make_wwtc(hw_type: u16, device: u8, family: u8, hwver: u16, maj: u8, min: u8, payload: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&hw_type.to_le_bytes());
        b.push(device);
        b.push(family);
        b.extend_from_slice(&hwver.to_le_bytes());
        b.push(min);
        b.push(maj);
        b.extend_from_slice(payload);
        b.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        b.extend_from_slice(&crc16_modbus(payload).to_le_bytes());
        b
    }

    #[test]
    fn parse_valid_wwtc() {
        let payload: Vec<u8> = (0..64u8).collect(); // 64 = multiple de 4
        let bytes = make_wwtc(0x0122, 0x0a, 0xbf, 0x5100, 1, 2, &payload);
        let fw = parse_wwtc(&bytes).unwrap();
        assert_eq!(fw.header.device, 0x0a);
        assert_eq!(fw.header.family, 0xbf);
        assert_eq!(fw.header.hardware_version, 0x5100);
        assert_eq!(fw.header.version(), "1.02");
        assert_eq!(fw.payload, payload);
        assert_eq!(fw.n_data_frames(), 16);
    }

    #[test]
    fn parse_rejects_bad_crc() {
        let payload: Vec<u8> = (0..8u8).collect();
        let mut bytes = make_wwtc(0x0122, 0x0a, 0xbf, 0x5100, 1, 0, &payload);
        let n = bytes.len();
        bytes[n - 1] ^= 0xFF; // corrompt le CRC
        assert!(parse_wwtc(&bytes).unwrap_err().contains("CRC"));
    }

    #[test]
    fn parse_rejects_non_multiple_of_4() {
        let payload: Vec<u8> = (0..6u8).collect(); // 6 = pas multiple de 4
        let bytes = make_wwtc(0x0122, 0x0a, 0xbf, 0x5100, 1, 0, &payload);
        assert!(parse_wwtc(&bytes).unwrap_err().contains("multiple de 4"));
    }

    #[test]
    fn parse_rejects_length_mismatch() {
        let payload: Vec<u8> = (0..8u8).collect();
        let mut bytes = make_wwtc(0x0122, 0x0a, 0xbf, 0x5100, 1, 0, &payload);
        let n = bytes.len();
        bytes[n - 6] = 0xFF; // fausse la longueur du pied
        assert!(parse_wwtc(&bytes).is_err());
    }

    #[test]
    fn check_target_matches_and_rejects() {
        let payload: Vec<u8> = (0..8u8).collect();
        let bytes = make_wwtc(0x0122, 0x0a, 0xbf, 0x5100, 1, 2, &payload);
        let fw = parse_wwtc(&bytes).unwrap();
        let tgt = |device, family, hwver, hw_type| FlashTarget { device, family, hwver, hw_type };
        assert!(check_target(&fw, &tgt(0x0a, 0xbf, 0x5100, 0x0122)).is_ok());
        assert!(check_target(&fw, &tgt(0x20, 0xbb, 0x5100, 0x0123)).is_err()); // mauvais contrôleur
        assert!(check_target(&fw, &tgt(0x0a, 0xbf, 0x0110, 0x0122)).is_err()); // mauvaise HW version
    }

    #[test]
    fn data_chunks_are_contiguous() {
        let payload: Vec<u8> = (0..12u8).collect();
        let v: Vec<_> = data_chunks(&payload).collect();
        assert_eq!(v.len(), 3);
        assert_eq!(v[0], (0, [0, 1, 2, 3]));
        assert_eq!(v[1], (4, [4, 5, 6, 7]));
        assert_eq!(v[2], (8, [8, 9, 10, 11]));
        // Trame UPDATE_DATA : offset 3 o LE + 4 o data.
        assert_eq!(update_data_args(4, [4, 5, 6, 7]), [4, 0, 0, 4, 5, 6, 7]);
    }
}

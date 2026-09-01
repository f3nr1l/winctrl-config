//! Persistance des remaps bouton-par-bouton (D-8) — sans GTK.
//!
//! Un remap se réduit à un dict d'**overrides** `{ordinal_src: ordinal_dst}`
//! (1-based, bouton physique → bouton de sortie), celui-là même que
//! [`crate::remap::plan_remap`] consomme. On le stocke **par appareil** en JSON
//! sous `~/.local/share/winctrl/remap/`, sur le modèle de `remap_store.py`.
//!
//! La logique (modèle [`RemapMapping`], (dé)sérialisation, dérivation du chemin)
//! est **pure et testable** ; seules [`load_mapping`] / [`save_mapping`] touchent le
//! disque.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use nix::libc;
use serde_json::{json, Value};

use crate::enumerate::WinwingDevice;

const REMAP_FORMAT: &str = "winwing-remap";
const REMAP_VERSION: u64 = 1;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Modèle **pur** d'un remap : un dict d'overrides `{src: dst}` (1-based).
///
/// Poser `src == dst` **retire** l'override (l'identité n'a pas besoin d'être
/// stockée : `plan_remap` renvoie déjà le bouton à l'identique par défaut). Les
/// ordinaux sont validés `>= 1` ; la borne haute réelle (nombre de boutons du
/// manche) est vérifiée à la construction du plan, pas ici.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemapMapping {
    overrides: BTreeMap<u32, u32>,
}

impl RemapMapping {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ajoute/remplace la réaffectation `src -> dst`. `src == dst` la retire.
    pub fn set(&mut self, src: u32, dst: u32) -> Result<(), String> {
        if src < 1 || dst < 1 {
            return Err(format!("remap invalide : {src}->{dst} (ordinaux >= 1)"));
        }
        if src == dst {
            self.overrides.remove(&src);
        } else {
            self.overrides.insert(src, dst);
        }
        Ok(())
    }

    /// Retire la réaffectation du bouton `src` (sans erreur si absente).
    pub fn clear(&mut self, src: u32) {
        self.overrides.remove(&src);
    }

    pub fn clear_all(&mut self) {
        self.overrides.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.overrides.is_empty()
    }

    pub fn len(&self) -> usize {
        self.overrides.len()
    }

    /// Réaffectations triées par bouton source (pour l'affichage).
    pub fn entries(&self) -> impl Iterator<Item = (u32, u32)> + '_ {
        self.overrides.iter().map(|(&s, &d)| (s, d))
    }

    /// La sortie enregistrée d'un bouton source, ou le bouton lui-même (identité).
    pub fn output_for(&self, src: u32) -> u32 {
        *self.overrides.get(&src).unwrap_or(&src)
    }

    /// Copie prête pour [`crate::remap::plan_remap`].
    pub fn to_overrides(&self) -> HashMap<u32, u32> {
        self.overrides.iter().map(|(&s, &d)| (s, d)).collect()
    }

    fn to_json(&self, device_meta: Value) -> Value {
        let overrides: serde_json::Map<String, Value> = self
            .overrides
            .iter()
            .map(|(&s, &d)| (s.to_string(), json!(d)))
            .collect();
        json!({
            "format": REMAP_FORMAT,
            "version": REMAP_VERSION,
            "app_version": APP_VERSION,
            "device": device_meta,
            "overrides": overrides,
        })
    }

    fn from_json(obj: &Value) -> Result<Self, String> {
        if obj.get("format").and_then(Value::as_str) != Some(REMAP_FORMAT) {
            return Err("format de remap inattendu".into());
        }
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version > REMAP_VERSION {
            return Err(format!("version de remap trop récente : {version}"));
        }
        let raw = obj
            .get("overrides")
            .and_then(Value::as_object)
            .ok_or("remap sans table d'overrides")?;
        let mut m = RemapMapping::new();
        for (k, v) in raw {
            let src: u32 = k.parse().map_err(|_| "clé d'override non entière")?;
            let dst = v.as_u64().ok_or("valeur d'override non entière")? as u32;
            m.set(src, dst)?;
        }
        Ok(m)
    }
}

// --- métadonnées / chemin (purs) ------------------------------------------
/// Métadonnées légères d'appareil, pour retrouver l'origine d'un remap.
fn device_meta(dev: &WinwingDevice) -> Value {
    json!({
        "pid": format!("{:04x}", dev.pid),
        "serial": dev.serial,
        "product_name": dev.product,
    })
}

/// Clé stable d'appareil : PID + numéro de série (s'il existe), pour que deux
/// manches identiques (même PID) gardent des remaps distincts.
pub fn device_key(dev: &WinwingDevice) -> String {
    let pid = format!("{:04x}", dev.pid);
    if dev.serial.is_empty() {
        pid
    } else {
        format!("{pid}-{}", dev.serial)
    }
}

pub(crate) fn slug(key: &str) -> String {
    let mut out = String::with_capacity(key.len());
    let mut last_dash = false;
    for ch in key.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "remap".to_string()
    } else {
        trimmed
    }
}

/// Home de l'utilisateur réel — même sous sudo, on vise SON home, pas `/root`.
pub(crate) fn user_home() -> PathBuf {
    if let Ok(sudo_user) = std::env::var("SUDO_USER") {
        // SAFETY : geteuid ne prend pas d'argument.
        if !sudo_user.is_empty() && unsafe { libc::geteuid() } == 0 {
            if let Some(home) = passwd_field(&sudo_user, PasswdField::Dir) {
                return PathBuf::from(home);
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() {
            return PathBuf::from(home);
        }
    }
    PathBuf::from(".")
}

pub fn remap_dir() -> PathBuf {
    user_home().join(".local/share/winctrl/remap")
}

pub fn mapping_path(dev: &WinwingDevice, directory: Option<&Path>) -> PathBuf {
    let dir = directory.map(Path::to_path_buf).unwrap_or_else(remap_dir);
    dir.join(format!("{}.json", slug(&device_key(dev))))
}

// --- I/O disque ------------------------------------------------------------
/// Charge le remap enregistré de l'appareil, ou un remap **vide** si aucun fichier
/// (ou fichier illisible/corrompu) — l'ouverture de la page ne doit jamais échouer
/// pour ça.
pub fn load_mapping(dev: &WinwingDevice, directory: Option<&Path>) -> RemapMapping {
    let path = mapping_path(dev, directory);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => RemapMapping::from_json(&v).unwrap_or_default(),
            Err(_) => RemapMapping::new(),
        },
        Err(_) => RemapMapping::new(),
    }
}

/// Écrit le remap en JSON. Un remap **vide** supprime le fichier plutôt que de
/// laisser un squelette. Restitue les fichiers à l'utilisateur réel sous sudo.
pub fn save_mapping(
    dev: &WinwingDevice,
    mapping: &RemapMapping,
    directory: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let dir = directory.map(Path::to_path_buf).unwrap_or_else(remap_dir);
    let path = mapping_path(dev, Some(&dir));
    if mapping.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(&mapping.to_json(device_meta(dev)))?;
    std::fs::write(&path, text)?;
    // Chowner AUSSI le dossier de base `…/winwing` (parent de `remap/`) : sous sudo
    // il peut venir d'être créé en root, bloquant un run non-sudo ultérieur.
    if let Some(remap_parent) = dir.parent() {
        chown_to_user(&[remap_parent, &dir, &path]);
    } else {
        chown_to_user(&[&dir, &path]);
    }
    Ok(path)
}

// --- sudo : restitution des fichiers à l'utilisateur réel ------------------
enum PasswdField {
    Uid,
    Gid,
    Dir,
}

/// Lit un champ du `passwd` de `name` via `getpwnam`. `None` si l'utilisateur est
/// inconnu. (FFI directe : `nix` est compilé sans la feature `user`.)
fn passwd_field(name: &str, field: PasswdField) -> Option<String> {
    let cname = std::ffi::CString::new(name).ok()?;
    // SAFETY : cname valide ; le pointeur renvoyé pointe une struct statique du
    // libc, valide jusqu'au prochain appel getpw* (on la lit immédiatement).
    let pw = unsafe { libc::getpwnam(cname.as_ptr()) };
    if pw.is_null() {
        return None;
    }
    // SAFETY : pw non nul, champs valides tant qu'aucun autre getpw* n'est appelé.
    unsafe {
        match field {
            PasswdField::Uid => Some((*pw).pw_uid.to_string()),
            PasswdField::Gid => Some((*pw).pw_gid.to_string()),
            PasswdField::Dir => {
                let dir = (*pw).pw_dir;
                if dir.is_null() {
                    None
                } else {
                    Some(std::ffi::CStr::from_ptr(dir).to_string_lossy().into_owned())
                }
            }
        }
    }
}

/// Sous sudo, rend les fichiers à l'utilisateur réel (pas root). Best-effort.
pub(crate) fn chown_to_user(paths: &[&Path]) {
    let sudo_user = match std::env::var("SUDO_USER") {
        Ok(u) if !u.is_empty() => u,
        _ => return,
    };
    // SAFETY : geteuid sans argument.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    let (uid, gid) = match (
        passwd_field(&sudo_user, PasswdField::Uid),
        passwd_field(&sudo_user, PasswdField::Gid),
    ) {
        (Some(u), Some(g)) => (
            u.parse::<u32>().unwrap_or(u32::MAX),
            g.parse::<u32>().unwrap_or(u32::MAX),
        ),
        _ => return,
    };
    if uid == u32::MAX || gid == u32::MAX {
        return;
    }
    for p in paths {
        if let Ok(cpath) = std::ffi::CString::new(p.as_os_str().to_string_lossy().as_bytes()) {
            // SAFETY : cpath valide ; chown best-effort (échec ignoré).
            unsafe {
                libc::chown(cpath.as_ptr(), uid, gid);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enumerate::Controller;

    fn dev(pid: u16, serial: &str) -> WinwingDevice {
        WinwingDevice {
            hidraw: "/dev/hidraw4".into(),
            vid: 0x4098,
            pid,
            product: "URSA MINOR R".into(),
            serial: serial.into(),
            controllers: vec![Controller::new(0x0A, 0xBF)],
            evdev: "/dev/input/event20".into(),
        }
    }

    #[test]
    fn set_identity_removes_override() {
        let mut m = RemapMapping::new();
        m.set(3, 5).unwrap();
        assert_eq!(m.output_for(3), 5);
        m.set(3, 3).unwrap(); // identité -> retrait
        assert!(m.is_empty());
    }

    #[test]
    fn rejects_zero_ordinals() {
        let mut m = RemapMapping::new();
        assert!(m.set(0, 5).is_err());
        assert!(m.set(2, 0).is_err());
    }

    #[test]
    fn device_key_uses_serial_when_present() {
        assert_eq!(device_key(&dev(0xBC2A, "SN123")), "bc2a-SN123");
        assert_eq!(device_key(&dev(0xBC2A, "")), "bc2a");
    }

    #[test]
    fn slug_sanitizes() {
        assert_eq!(slug("bc2a-SN 12/3"), "bc2a-SN-12-3");
        assert_eq!(slug("///"), "remap");
    }

    #[test]
    fn json_roundtrip() {
        let mut m = RemapMapping::new();
        m.set(1, 5).unwrap();
        m.set(2, 6).unwrap();
        let v = m.to_json(device_meta(&dev(0xBC2A, "SN")));
        let back = RemapMapping::from_json(&v).unwrap();
        assert_eq!(back, m);
        assert_eq!(v["format"], REMAP_FORMAT);
        assert_eq!(v["overrides"]["1"], 5);
    }

    #[test]
    fn save_then_load_roundtrip_via_tmp() {
        let dir = std::env::temp_dir().join(format!("winwing-remap-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let d = dev(0xBC2A, "SNROUND");
        let mut m = RemapMapping::new();
        m.set(4, 9).unwrap();
        save_mapping(&d, &m, Some(&dir)).unwrap();
        let back = load_mapping(&d, Some(&dir));
        assert_eq!(back, m);
        // vide -> supprime le fichier
        let empty = RemapMapping::new();
        save_mapping(&d, &empty, Some(&dir)).unwrap();
        assert!(load_mapping(&d, Some(&dir)).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}

//! Découverte des périphériques WinWing branchés, via `/sys` uniquement.
//!
//! Ne fait aucune I/O hidraw (donc ne demande aucune permission device) : lit les
//! `uevent` de la classe hidraw pour trouver les endpoints de VID 4098, en déduit
//! les contrôleurs (poignée + base) et rattache le numéro de série USB.

use std::collections::HashMap;
use std::fmt;
use std::path::Path;

use crate::protocol as p;

pub const VID_WINWING: u16 = 0x4098;

// --- access(2) : test de permission SANS ouvrir le device -----------------
// `os.access(path, R_OK|W_OK)` de Python consulte les permissions effectives (y
// compris l'ACL POSIX que la règle udev `uaccess` pose sur VID 4098) sans ouvrir
// le nœud — indispensable pour rester « aucune I/O device » à l'énumération. La
// std Rust n'expose pas access(2) ; on l'appelle en FFI directe sur la libc déjà
// liée (aucune dépendance de crate ajoutée).
mod ffi {
    use std::os::raw::{c_char, c_int};
    extern "C" {
        pub fn access(path: *const c_char, amode: c_int) -> c_int;
    }
    pub const R_OK: c_int = 4;
    pub const W_OK: c_int = 2;
}

/// Test de permission effective (ACL comprise) sans ouvrir le nœud.
fn can_access(path: &str, mode: std::os::raw::c_int) -> bool {
    match std::ffi::CString::new(path) {
        Ok(c) => unsafe { ffi::access(c.as_ptr(), mode) == 0 },
        Err(_) => false,
    }
}

/// Un contrôleur logique adressable sur un endpoint `(device, family)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Controller {
    pub device: u8,
    pub family: u8,
}

impl Controller {
    pub fn new(device: u8, family: u8) -> Self {
        Controller { device, family }
    }

    /// Nom de modèle, ou `"?"` si inconnu.
    pub fn model(&self) -> &'static str {
        p::controller_name(self.device, self.family).unwrap_or("?")
    }

    /// « pid » de l'API catalogue firmware = `(family << 8) | device`.
    pub fn pid_api(&self) -> u16 {
        p::controller_pid(self.device, self.family)
    }
}

impl fmt::Display for Controller {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (dev {:#04x}/fam {:#04x})",
            self.model(),
            self.device,
            self.family
        )
    }
}

/// Un périphérique WinWing = un endpoint hidraw = plusieurs contrôleurs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WinwingDevice {
    /// `/dev/hidrawN`.
    pub hidraw: String,
    pub vid: u16,
    pub pid: u16,
    pub product: String,
    pub serial: String,
    pub controllers: Vec<Controller>,
    /// `/dev/input/eventN` du joystick (moniteur live), si trouvé.
    pub evdev: String,
}

impl WinwingDevice {
    /// L'endpoint est-il ouvrable en lecture/écriture par l'utilisateur ?
    pub fn readable(&self) -> bool {
        can_access(&self.hidraw, ffi::R_OK | ffi::W_OK)
    }

    /// Le nœud evdev (moniteur live) est-il lisible ?
    pub fn live_readable(&self) -> bool {
        !self.evdev.is_empty() && can_access(&self.evdev, ffi::R_OK)
    }
}

fn parse_uevent(path: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(path) {
        for line in text.lines() {
            if let Some((k, v)) = line.split_once('=') {
                out.insert(k.to_string(), v.to_string());
            }
        }
    }
    out
}

/// Liste les sous-entrées d'un répertoire dont le nom commence par `prefix`,
/// triées par nom (ordre lexicographique, comme `sorted(glob(...))` de Python).
fn sorted_entries(dir: &str, prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with(prefix))
            .collect(),
        Err(_) => Vec::new(),
    };
    names.sort();
    names
}

/// Lit un fichier sysfs et le parse comme entier hexadécimal.
fn read_hex(path: &Path) -> Option<u32> {
    let text = std::fs::read_to_string(path).ok()?;
    u32::from_str_radix(text.trim(), 16).ok()
}

/// Cherche le iSerial du device USB `(vid:pid)` dans `/sys/bus/usb`.
fn usb_serial(vid: u16, pid: u16) -> String {
    let root = "/sys/bus/usb/devices";
    for name in sorted_entries(root, "") {
        let base = Path::new(root).join(&name);
        let idv = base.join("idVendor");
        if !idv.exists() {
            continue;
        }
        if read_hex(&idv) != Some(vid as u32) {
            continue;
        }
        if read_hex(&base.join("idProduct")) != Some(pid as u32) {
            continue;
        }
        if let Ok(s) = std::fs::read_to_string(base.join("serial")) {
            return s.trim().to_string();
        }
    }
    String::new()
}

/// Nœud `/dev/input/eventN` du joystick `(vid:pid)`, via `/sys` — aucune I/O
/// device. Préfère un nœud portant des axes absolus (ABS), pour éviter un
/// éventuel nœud secondaire sans axes. `""` si rien trouvé.
pub fn find_event_node(vid: u16, pid: u16) -> String {
    let mut fallback = String::new();
    for name in sorted_entries("/sys/class/input", "event") {
        let ev = Path::new("/sys/class/input").join(&name);
        let iddir = ev.join("device").join("id");
        if read_hex(&iddir.join("vendor")) != Some(vid as u32) {
            continue;
        }
        if read_hex(&iddir.join("product")) != Some(pid as u32) {
            continue;
        }
        let node = format!("/dev/input/{name}");
        // a des axes absolus ? -> c'est le bon nœud
        let has_abs = std::fs::read_to_string(ev.join("device/capabilities/abs"))
            .ok()
            .and_then(|s| u128::from_str_radix(s.trim(), 16).ok())
            .map(|v| v != 0)
            .unwrap_or(false);
        if has_abs {
            return node;
        }
        if fallback.is_empty() {
            fallback = node;
        }
    }
    fallback
}

/// Contrôleurs attendus pour un PID : poignée (déduite du PID) + base.
fn controllers_for(pid: u16) -> Vec<Controller> {
    let mut ctrls = Vec::new();
    let grip = p::grip_device_from_pid(pid);
    if (0..=0xFF).contains(&grip)
        && p::controller_name(grip as u8, p::FAMILY_GRIP).is_some()
    {
        ctrls.push(Controller::new(grip as u8, p::FAMILY_GRIP));
    }
    ctrls.push(Controller::new(p::DEVICE_BASE, p::FAMILY_BASE));
    ctrls
}

/// Liste les périphériques WinWing branchés, triés par chemin hidraw.
pub fn discover() -> Vec<WinwingDevice> {
    let mut devices = Vec::new();
    let root = "/sys/class/hidraw";
    for name in sorted_entries(root, "hidraw") {
        let sysdir = Path::new(root).join(&name);
        let ue = parse_uevent(&sysdir.join("device").join("uevent"));
        // HID_ID ex. 0003:00004098:0000BC2A
        let hid_id = match ue.get("HID_ID") {
            Some(v) => v,
            None => continue,
        };
        let parts: Vec<&str> = hid_id.split(':').collect();
        if parts.len() != 3 {
            continue;
        }
        let (vid, pid) = match (
            u32::from_str_radix(parts[1], 16),
            u32::from_str_radix(parts[2], 16),
        ) {
            (Ok(v), Ok(p)) => ((v & 0xFFFF) as u16, (p & 0xFFFF) as u16),
            _ => continue,
        };
        if vid != VID_WINWING {
            continue;
        }
        devices.push(WinwingDevice {
            hidraw: format!("/dev/{name}"),
            vid,
            pid,
            product: ue.get("HID_NAME").cloned().unwrap_or_default(),
            serial: usb_serial(vid, pid),
            controllers: controllers_for(pid),
            evdev: find_event_node(vid, pid),
        });
    }
    devices
}

/// Rendu texte de la découverte, pour le mode CLI.
pub fn format_list(devices: &[WinwingDevice]) -> String {
    use crate::i18n::{tr, tr_f};
    if devices.is_empty() {
        return tr("No WinWing device detected.");
    }
    let mut lines: Vec<String> = Vec::new();
    for d in devices {
        let acc = if d.readable() {
            String::new()
        } else {
            format!("  ({})", tr("not accessible — install the access rule"))
        };
        lines.push(format!(
            "{}  {:04x}:{:04x}  {}{}",
            d.hidraw, d.vid, d.pid, d.product, acc
        ));
        if !d.serial.is_empty() {
            lines.push(format!("    {}", tr_f("USB serial: {}", &[d.serial.as_str()])));
        }
        for c in &d.controllers {
            lines.push(format!("    • {c}"));
        }
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controllers_for_grip_r() {
        // PID BC2A -> poignée F1_R (device 0x0A) + base
        let ctrls = controllers_for(0xBC2A);
        assert_eq!(ctrls.len(), 2);
        assert_eq!(ctrls[0], Controller::new(0x0A, p::FAMILY_GRIP));
        assert_eq!(ctrls[0].model(), "JGRIP_F1_R");
        assert_eq!(ctrls[1], Controller::new(p::DEVICE_BASE, p::FAMILY_BASE));
        assert_eq!(ctrls[1].model(), "J5_BASE");
    }

    #[test]
    fn controllers_for_unknown_grip_falls_back_to_base_only() {
        // PID dont l'octet bas ne correspond à aucune poignée connue -> base seule
        let ctrls = controllers_for(0x0000);
        assert_eq!(ctrls.len(), 1);
        assert_eq!(ctrls[0].family, p::FAMILY_BASE);
    }

    #[test]
    fn controller_display_and_pid_api() {
        let c = Controller::new(0x0A, p::FAMILY_GRIP);
        assert_eq!(c.to_string(), "JGRIP_F1_R (dev 0x0a/fam 0xbf)");
        assert_eq!(c.pid_api(), 0xBF0A);
    }

    #[test]
    fn format_list_empty() {
        assert_eq!(format_list(&[]), "No WinWing device detected.");
    }
}

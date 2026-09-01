//! Moniteur d'entrées live (domaine D-2) — lecture evdev brute.
//!
//! Le « Test » de SimApp Pro : voir en direct la valeur des axes et l'état des
//! boutons. **On ne lit PAS la trame vendor** — axes et boutons sont exposés par
//! le pilote noyau sur un nœud `/dev/input/eventN` (le joystick standard). On lit
//! donc ce nœud, sans dépendance lourde : on décode nous-mêmes la struct
//! `input_event` et on interroge les capacités par `ioctl` (mêmes appels que le
//! noyau, aucun paquet à installer).
//!
//! Cette couche est **sans GTK** : elle ouvre le fd, découvre axes/boutons, et
//! décode les événements en mettant à jour un état. L'UI se contente de surveiller
//! le fd (`glib::source::unix_fd_add_local`) et d'appeler [`LiveInput::poll`] puis
//! de peindre l'état — l'I/O ne bloque jamais le thread UI (lecture non bloquante,
//! drainée à chaque réveil).
//!
//! Plages relevées sur une URSA MINOR : `ABS_X/Y/Z` 0–65535, `ABS_RX/RY` et
//! `ABS_THROTTLE` 0–4095, `ABS_HAT0X/Y` −1..1, boutons à partir de `BTN_JOYSTICK`
//! (0x120).

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::RawFd;

use nix::libc;

// --- struct input_event (linux/input.h) -----------------------------------
// struct { struct timeval time; __u16 type; __u16 code; __s32 value; }
// timeval sur LP64 = 2×i64 (tv_sec, tv_usec). Taille totale = 16 + 2 + 2 + 4 = 24.
pub const EV_SIZE: usize = 24;

pub const EV_SYN: u16 = 0x00;
pub const EV_KEY: u16 = 0x01;
pub const EV_ABS: u16 = 0x03;

// --- codes ABS connus (linux/input-event-codes.h) -------------------------
pub const ABS_X: u16 = 0x00;
pub const ABS_Y: u16 = 0x01;
pub const ABS_Z: u16 = 0x02;
pub const ABS_RX: u16 = 0x03;
pub const ABS_RY: u16 = 0x04;
pub const ABS_RZ: u16 = 0x05;
pub const ABS_THROTTLE: u16 = 0x06;
pub const ABS_HAT0X: u16 = 0x10;
pub const ABS_HAT0Y: u16 = 0x11;
pub const ABS_MAX: u16 = 0x3F;

/// Premier code bouton d'un joystick.
pub const BTN_JOYSTICK: u16 = 0x120;

/// Boutons **réels** de l'URSA MINOR : le descripteur HID en déclare ~111, mais
/// seuls les ~48-51 premiers sont câblés ; 52-111 sont du padding fantôme. On borne
/// la découverte à ce nombre pour ne jamais exposer les fantômes.
pub const MAX_REAL_BUTTONS: usize = 51;
/// Dernier code clavier/bouton adressable (linux/input-event-codes.h).
pub const KEY_MAX: u16 = 0x2FF;

/// Libellé lisible d'un code ABS : (nom court technique, description, centré ?).
/// `centered` = axe à rappel (repère de centre à l'affichage). Un code inconnu
/// retombe sur `ABS_xx` sans description.
fn abs_label(code: u16) -> (String, &'static str, bool) {
    // La description est une chaîne source anglaise, traduite au point d'affichage.
    let (name, desc, centered) = match code {
        ABS_X => ("X", "roll", true),
        ABS_Y => ("Y", "pitch", true),
        ABS_Z => ("Z", "yaw", true),
        ABS_RX => ("Rx", "mini-stick", false),
        ABS_RY => ("Ry", "mini-stick", false),
        ABS_RZ => ("Rz", "yaw", true),
        ABS_THROTTLE => ("Slider", "throttle", false),
        ABS_HAT0X => ("Hat", "cross (X)", false),
        ABS_HAT0Y => ("Hat", "cross (Y)", false),
        _ => return (format!("ABS_{code:02x}"), "", false),
    };
    (name.to_string(), desc, centered)
}

// --- ioctl (encodage identique au noyau) ----------------------------------
const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}

const IOC_READ: u64 = 2;
const E: u64 = b'E' as u64;

const fn eviocgbit(ev: u16, len: u64) -> u64 {
    ioc(IOC_READ, E, 0x20 + ev as u64, len)
}

const fn eviocgabs(abs: u16) -> u64 {
    // input_absinfo = 6 × __s32 (value, min, max, fuzz, flat, resolution)
    ioc(IOC_READ, E, 0x40 + abs as u64, 24)
}

/// `EVIOCGNAME(len)` — nom du device physique.
pub(crate) const fn eviocgname(len: u64) -> u64 {
    ioc(IOC_READ, E, 0x06, len)
}

/// Lit un buffer via `ioctl`. Erreur système en cas d'échec.
fn ioctl_read(fd: RawFd, request: u64, buf: &mut [u8]) -> io::Result<()> {
    // SAFETY : fd valide, request = ioctl de lecture, buf assez grand pour la
    // taille encodée dans la request.
    let rc = unsafe { libc::ioctl(fd, request as libc::c_ulong, buf.as_mut_ptr()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Décode un tampon brut en liste de `(type, code, value)`. Ignore un reliquat
/// partiel (lecture toujours alignée sur `EV_SIZE` en pratique).
pub fn parse_events(buf: &[u8]) -> Vec<(u16, u16, i32)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + EV_SIZE <= buf.len() {
        // offsets : type @16, code @18, value @20 (timeval @0..16 ignoré).
        let etype = u16::from_ne_bytes([buf[i + 16], buf[i + 17]]);
        let code = u16::from_ne_bytes([buf[i + 18], buf[i + 19]]);
        let value = i32::from_ne_bytes([buf[i + 20], buf[i + 21], buf[i + 22], buf[i + 23]]);
        out.push((etype, code, value));
        i += EV_SIZE;
    }
    out
}

/// État d'un axe : métadonnées + dernière valeur.
#[derive(Debug, Clone)]
pub struct AxisState {
    pub code: u16,
    /// Identifiant technique court (« X », « Rx »… non traduit).
    pub name: String,
    /// Description lisible (vide si code inconnu).
    pub desc: &'static str,
    pub minimum: i32,
    pub maximum: i32,
    pub value: i32,
    pub centered: bool,
}

impl AxisState {
    pub fn span(&self) -> i32 {
        self.maximum - self.minimum
    }

    /// Position 0..1 dans la plage (bornée).
    pub fn fraction(&self) -> f64 {
        let span = self.span();
        if span <= 0 {
            return 0.0;
        }
        let f = (self.value - self.minimum) as f64 / span as f64;
        f.clamp(0.0, 1.0)
    }

    pub fn display(&self) -> String {
        format!("{} / {}", self.value, self.maximum)
    }
}

/// Instantané de l'état live, indépendant de GTK (facilite les tests).
#[derive(Debug, Clone, Default)]
pub struct LiveState {
    pub axes: Vec<AxisState>,
    /// Codes evdev des boutons, dans l'ordre de découverte (ordinal d'affichage).
    pub buttons: Vec<u16>,
    /// Codes actuellement pressés.
    pub pressed: HashSet<u16>,
}

impl LiveState {
    /// Numéro 1-based du bouton (ordinal d'affichage), ou -1 si inconnu.
    pub fn button_index(&self, code: u16) -> i32 {
        match self.buttons.iter().position(|&c| c == code) {
            Some(i) => i as i32 + 1,
            None => -1,
        }
    }
}

/// Ouvre un nœud evdev et décode axes/boutons en continu (non bloquant).
///
/// Cycle de vie : `open` → l'UI surveille `fileno()` → à chaque réveil `poll()`
/// draine les événements et met à jour `state` → `Drop` ferme le fd.
pub struct LiveInput {
    fd: RawFd,
    pub state: LiveState,
    /// code ABS -> index dans `state.axes`.
    axis_index: HashMap<u16, usize>,
}

impl LiveInput {
    /// Ouvre le nœud et découvre axes/boutons.
    pub fn open(path: &str) -> io::Result<Self> {
        let cpath = std::ffi::CString::new(path)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chemin nul"))?;
        // SAFETY : cpath valide, flags standards.
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut li = LiveInput {
            fd,
            state: LiveState::default(),
            axis_index: HashMap::new(),
        };
        li.discover_axes();
        li.discover_buttons();
        Ok(li)
    }

    fn discover_axes(&mut self) {
        let nbytes = (ABS_MAX as usize / 8) + 1;
        let mut bits = vec![0u8; nbytes];
        if ioctl_read(self.fd, eviocgbit(EV_ABS, nbytes as u64), &mut bits).is_err() {
            return;
        }
        for code in 0..ABS_MAX {
            let c = code as usize;
            if (bits[c / 8] >> (c % 8)) & 1 == 0 {
                continue;
            }
            let (mut mn, mut mx, mut val) = (0i32, 65535i32, 0i32);
            let mut info = [0u8; 24];
            if ioctl_read(self.fd, eviocgabs(code), &mut info).is_ok() {
                val = i32::from_ne_bytes([info[0], info[1], info[2], info[3]]);
                mn = i32::from_ne_bytes([info[4], info[5], info[6], info[7]]);
                mx = i32::from_ne_bytes([info[8], info[9], info[10], info[11]]);
            }
            let (name, desc, centered) = abs_label(code);
            self.axis_index.insert(code, self.state.axes.len());
            self.state.axes.push(AxisState {
                code,
                name,
                desc,
                minimum: mn,
                maximum: mx,
                value: val,
                centered,
            });
        }
    }

    fn discover_buttons(&mut self) {
        let nbytes = (KEY_MAX as usize / 8) + 1;
        let mut bits = vec![0u8; nbytes];
        if ioctl_read(self.fd, eviocgbit(EV_KEY, nbytes as u64), &mut bits).is_err() {
            return;
        }
        for code in 0..KEY_MAX {
            let c = code as usize;
            if (bits[c / 8] >> (c % 8)) & 1 == 1 {
                self.state.buttons.push(code);
                // L'URSA MINOR déclare ~111 boutons HID mais n'en câble que ~48 : les
                // ordinaux au-delà de MAX_REAL_BUTTONS sont du **padding fantôme** (ne
                // s'actionnent jamais). On les ignore partout (moniteur, remap, split).
                if self.state.buttons.len() >= MAX_REAL_BUTTONS {
                    break;
                }
            }
        }
    }

    /// Draine tous les événements en attente et met à jour l'état. Renvoie `true`
    /// si au moins un événement a été lu. Non bloquant.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut buf = [0u8; EV_SIZE * 64];
        loop {
            // SAFETY : fd valide, buf assez grand.
            let n = unsafe {
                libc::read(self.fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len())
            };
            if n <= 0 {
                break; // EAGAIN (rien à lire), EOF, ou erreur : on s'arrête.
            }
            let n = n as usize;
            for (etype, code, value) in parse_events(&buf[..n]) {
                match etype {
                    EV_ABS => {
                        if let Some(&idx) = self.axis_index.get(&code) {
                            self.state.axes[idx].value = value;
                            changed = true;
                        }
                    }
                    EV_KEY => {
                        if value != 0 {
                            self.state.pressed.insert(code);
                        } else {
                            self.state.pressed.remove(&code);
                        }
                        changed = true;
                    }
                    _ => {}
                }
            }
            if n < buf.len() {
                break;
            }
        }
        changed
    }

    pub fn fileno(&self) -> RawFd {
        self.fd
    }
}

impl Drop for LiveInput {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY : fd ouvert par nous, fermé une seule fois (Drop).
            unsafe { libc::close(self.fd) };
            self.fd = -1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_roundtrip() {
        // Deux événements : ABS_X=1234 puis SYN.
        let mut buf = Vec::new();
        let mut push = |etype: u16, code: u16, value: i32| {
            buf.extend_from_slice(&0i64.to_ne_bytes()); // tv_sec
            buf.extend_from_slice(&0i64.to_ne_bytes()); // tv_usec
            buf.extend_from_slice(&etype.to_ne_bytes());
            buf.extend_from_slice(&code.to_ne_bytes());
            buf.extend_from_slice(&value.to_ne_bytes());
        };
        push(EV_ABS, ABS_X, 1234);
        push(EV_SYN, 0, 0);
        let evs = parse_events(&buf);
        assert_eq!(evs, vec![(EV_ABS, ABS_X, 1234), (EV_SYN, 0, 0)]);
    }

    #[test]
    fn parse_ignores_partial_tail() {
        let mut buf = vec![0u8; EV_SIZE + 5]; // un event complet + 5 octets
        buf[16] = EV_KEY as u8;
        let evs = parse_events(&buf);
        assert_eq!(evs.len(), 1);
    }

    #[test]
    fn axis_fraction_clamped() {
        let ax = AxisState {
            code: ABS_X,
            name: "X".into(),
            desc: "roll",
            minimum: 0,
            maximum: 100,
            value: 150,
            centered: true,
        };
        assert_eq!(ax.fraction(), 1.0);
        assert_eq!(ax.display(), "150 / 100");
    }

    #[test]
    fn button_index_1based() {
        let st = LiveState {
            buttons: vec![0x120, 0x121, 0x122],
            ..Default::default()
        };
        assert_eq!(st.button_index(0x121), 2);
        assert_eq!(st.button_index(0x999), -1);
    }
}

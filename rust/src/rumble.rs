//! Test du moteur de vibration par l'API **force feedback** du noyau (`FF_RUMBLE`).
//!
//! Contrairement à un effet de vibration (une *courbe* pilotée en vol par la
//! télémétrie d'un simulateur — cf. [`crate::vibration`]), ceci commande le moteur
//! **directement**, pour le *sentir* : on téléverse un effet `FF_RUMBLE` sur le
//! nœud evdev du manche (`EVIOCSFF`) puis on le joue (`EV_FF`). Port fidèle de
//! `tools/ff-test.py`, validé sur URSA. Réversible, aucune écriture flash.
//!
//! [`build_rumble_effect`] est **pure** (testable) ; [`Rumble`] touche le noyau.

use std::io;
use std::os::fd::RawFd;

use nix::libc;

const EV_FF: u16 = 0x15;
const FF_RUMBLE: u16 = 0x50;

/// `struct ff_effect` : 14 octets utiles, union alignée sur 8 → 48 octets au total.
const FF_EFFECT_SIZE: usize = 48;
/// `_IOW('E', 0x80, struct ff_effect)` (taille 0x30 = 48) — identique à `ff-test.py`.
const EVIOCSFF: libc::c_ulong = 0x4030_4580;
/// `_IOW('E', 0x81, int)` — retrait d'un effet, id **par valeur**.
const EVIOCRMFF: libc::c_ulong = 0x4004_4581;
const EV_SIZE: usize = 24; // input_event LP64 : timeval(16) + type(2) + code(2) + value(4)

/// Sérialise un `struct ff_effect` de type `FF_RUMBLE`. `effect_id = -1` demande au
/// noyau d'en attribuer un (relu après `EVIOCSFF`).
pub fn build_rumble_effect(strong: u16, weak: u16, length_ms: u16, effect_id: i16) -> [u8; FF_EFFECT_SIZE] {
    let mut b = [0u8; FF_EFFECT_SIZE];
    b[0..2].copy_from_slice(&FF_RUMBLE.to_ne_bytes()); // type
    b[2..4].copy_from_slice(&effect_id.to_ne_bytes()); // id
    b[4..6].copy_from_slice(&0u16.to_ne_bytes()); // direction
    // trigger { button:u16 @6, interval:u16 @8 } = 0
    // replay { length:u16 @10, delay:u16 @12 }
    b[10..12].copy_from_slice(&length_ms.to_ne_bytes());
    // union @16 : ff_rumble_effect { strong_magnitude:u16, weak_magnitude:u16 }
    b[16..18].copy_from_slice(&strong.to_ne_bytes());
    b[18..20].copy_from_slice(&weak.to_ne_bytes());
    b
}

/// `pct` (0..=100) -> magnitude `FF_RUMBLE` (0..=65535).
pub fn percent_to_magnitude(pct: f64) -> u16 {
    (pct.clamp(0.0, 100.0) / 100.0 * 65535.0).round() as u16
}

/// Un effet `FF_RUMBLE` téléversé sur un nœud evdev. `Drop` arrête le moteur et
/// retire l'effet — indispensable pour ne pas le laisser tourner.
pub struct Rumble {
    fd: RawFd,
    id: i16,
    uploaded: bool,
}

impl Rumble {
    /// Ouvre le nœud evdev en lecture/écriture et téléverse un effet `FF_RUMBLE`
    /// (magnitude 0..=65535, durée en ms). Le noyau arrête seul le moteur au bout de
    /// `length_ms` ; `Drop` garantit l'arrêt et le retrait.
    pub fn upload(evdev: &str, magnitude: u16, length_ms: u16) -> io::Result<Self> {
        let cpath = std::ffi::CString::new(evdev).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY : chemin valide ; O_RDWR requis pour EVIOCSFF + écriture d'events.
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_RDWR) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let mut r = Rumble { fd, id: -1, uploaded: false };
        let mut buf = build_rumble_effect(magnitude, 0, length_ms, -1);
        // SAFETY : fd valide, buf de FF_EFFECT_SIZE octets ; EVIOCSFF relit l'id @2.
        let rc = unsafe { libc::ioctl(fd, EVIOCSFF, buf.as_mut_ptr()) };
        if rc < 0 {
            return Err(io::Error::last_os_error()); // Drop ferme le fd
        }
        r.id = i16::from_ne_bytes([buf[2], buf[3]]);
        r.uploaded = true;
        Ok(r)
    }

    /// Lance le moteur (l'effet joue jusqu'à `length_ms` ou [`Self::stop`]).
    pub fn play(&self) -> io::Result<()> {
        self.send_ev(self.id as u16, 1)
    }

    /// Arrête le moteur (best-effort).
    pub fn stop(&self) {
        let _ = self.send_ev(self.id as u16, 0);
    }

    fn send_ev(&self, code: u16, value: i32) -> io::Result<()> {
        let mut ev = [0u8; EV_SIZE];
        ev[16..18].copy_from_slice(&EV_FF.to_ne_bytes());
        ev[18..20].copy_from_slice(&code.to_ne_bytes());
        ev[20..24].copy_from_slice(&value.to_ne_bytes());
        // SAFETY : fd valide, ev de EV_SIZE octets.
        let n = unsafe { libc::write(self.fd, ev.as_ptr() as *const libc::c_void, ev.len()) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for Rumble {
    fn drop(&mut self) {
        if self.fd >= 0 {
            if self.uploaded {
                self.stop();
                // SAFETY : EVIOCRMFF reçoit l'id par valeur.
                unsafe {
                    libc::ioctl(self.fd, EVIOCRMFF, self.id as libc::c_int);
                }
            }
            // SAFETY : fd ouvert par nous.
            unsafe {
                libc::close(self.fd);
            }
            self.fd = -1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_buffer_layout() {
        let b = build_rumble_effect(0x1234, 0x5678, 700, -1);
        assert_eq!(b.len(), 48);
        assert_eq!(u16::from_ne_bytes([b[0], b[1]]), FF_RUMBLE);
        assert_eq!(i16::from_ne_bytes([b[2], b[3]]), -1);
        assert_eq!(u16::from_ne_bytes([b[10], b[11]]), 700); // replay.length
        assert_eq!(u16::from_ne_bytes([b[16], b[17]]), 0x1234); // strong
        assert_eq!(u16::from_ne_bytes([b[18], b[19]]), 0x5678); // weak
    }

    #[test]
    fn magnitude_mapping() {
        assert_eq!(percent_to_magnitude(0.0), 0);
        assert_eq!(percent_to_magnitude(100.0), 65535);
        assert_eq!(percent_to_magnitude(50.0), 32768);
        assert_eq!(percent_to_magnitude(150.0), 65535); // borné
    }
}

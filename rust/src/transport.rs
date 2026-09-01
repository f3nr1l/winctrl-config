//! Transport HID brut vers un endpoint hidraw d'un périphérique WinWing.
//!
//! Un même `/dev/hidrawN` porte deux contrôleurs logiques (poignée + base) ; on
//! adresse l'un ou l'autre par le couple `(device, family)` passé à chaque appel.
//! Implémentation du `trait model::Transport` : open/close du fd hidraw (non
//! bloquant), write de la trame, read du report avec **écho-oracle** (filtre le
//! report joystick ID 1, opcode en `[6]`), timeouts, discipline mono-écrivain, et
//! `write_cfg` **gardé** (`protocol::guard_write` AVANT toute émission).
//!
//! I/O réelle : `std::fs::File` (RAII close) + `poll(2)` via `nix::libc` pour
//! l'attente à échéance. Le cœur pur (`protocol`) reste sans dépendance ; seul ce
//! transport tire `nix`.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::time::{Duration, Instant};

use nix::libc;

use crate::model::{Transport, TransportError};
use crate::protocol as p;

// Timeouts (cf. transport.py) : lecture ~0.4 s, restart ~0.6 s.
const READ_TIMEOUT: Duration = Duration::from_millis(400);
const RESTART_TIMEOUT: Duration = Duration::from_millis(600);

/// Ouvre un endpoint hidraw et échange des trames vendor report-id-2.
///
/// Discipline mono-écrivain : au plus un `HidrawTransport` ouvert par endpoint à
/// la fois (l'énumération, elle, ne l'ouvre jamais — elle lit `/sys`).
pub struct HidrawTransport {
    file: File,
}

impl HidrawTransport {
    fn send(&self, frame: &[u8]) -> io::Result<()> {
        // `impl Write for &File` : pas besoin d'emprunt mutable exclusif.
        (&self.file).write_all(frame)
    }

    /// Attend un report d'entrée ID 2 portant `opcode` en `[6]`. Filtre le report
    /// joystick ID 1 (bruit d'axes, surtout manche droite). `None` au timeout.
    ///
    /// C'est l'unique **oracle d'acceptation** : un opcode/offset inconnu est
    /// ignoré en silence par le firmware → absence d'écho = rejet.
    fn await_echo(&self, opcode: u8, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let fd = self.file.as_raw_fd();
        loop {
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let rem_ms = (deadline - now).as_millis().min(i32::MAX as u128) as i32;
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY : pfd est un pollfd valide, 1 descripteur, timeout en ms.
            let n = unsafe { libc::poll(&mut pfd, 1, rem_ms) };
            if n <= 0 {
                // 0 = tick de timeout ; -1 = EINTR → la deadline fait garde.
                continue;
            }
            let mut buf = [0u8; 64];
            match (&self.file).read(&mut buf) {
                Ok(len) if len >= 7 && buf[0] == p::REPORT_ID && buf[6] == opcode => {
                    return Some(buf[..len].to_vec());
                }
                // autre report (joystick ID 1, ou opcode différent) → on continue
                Ok(_) => continue,
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(_) => continue,
            }
        }
    }

    /// **Flashage seulement** ([`crate::flash`]) : émet une trame d'opcode bootloader
    /// (`0x40/0x20/0x21/0x22/0x23/0x24`) et attend son écho, rendant les **arguments**
    /// de l'accusé (charge utile après l'opcode), ou `None` au timeout. C'est la
    /// seule voie qui contourne la garde d'opcodes bannis — réservée au flash.
    pub fn flash_exchange(&self, frame: &[u8], opcode: u8, timeout: Duration) -> Option<Vec<u8>> {
        self.send(frame).ok()?;
        let resp = self.await_echo(opcode, timeout)?;
        if resp.len() < 6 {
            return Some(Vec::new());
        }
        let length = resp[5] as usize; // opcode inclus
        if length >= 1 {
            let end = (6 + length).min(resp.len());
            Some(resp[7..end].to_vec())
        } else {
            Some(Vec::new())
        }
    }

    /// SET_LEDX (0x49) : règle une LED/le moteur en **direct** (non-flash, aucun
    /// backup ni garde-fou identité requis — rien n'est écrit en flash).
    /// **Fire-and-forget** : n'attend pas l'écho (adapté à un slider live sans
    /// latence). `index` 0 = rétroéclairage (base) / moteur (poignée). Rend la
    /// trame émise.
    pub fn set_led(&self, device: u8, family: u8, index: u8, value: u8) -> io::Result<p::Frame> {
        let frame = p::build_frame(device, family, p::OP_SET_LEDX, &[index, value]);
        self.send(&frame)?;
        Ok(frame)
    }

    /// CALIBRATION_START (0x47) pour l'axe `index` : arme la capture (n'écrit
    /// RIEN en flash ; l'utilisateur balaie ensuite l'axe). Attend l'écho.
    pub fn start_calibration(&self, device: u8, family: u8, index: u8) -> io::Result<p::Frame> {
        let frame = p::build_frame(device, family, p::OP_CALIBRATION_START, &[index]);
        self.send(&frame)?;
        let _ = self.await_echo(p::OP_CALIBRATION_START, Duration::from_millis(500));
        Ok(frame)
    }

    /// CALIBRATION_FINISH (0x48) pour l'axe `index` : le FIRMWARE calcule et
    /// écrit min/centre/max dans 0xC8–0xF8 (pas l'hôte). Attend l'écho.
    pub fn finish_calibration(&self, device: u8, family: u8, index: u8) -> io::Result<p::Frame> {
        let frame = p::build_frame(device, family, p::OP_CALIBRATION_FINISH, &[index]);
        self.send(&frame)?;
        let _ = self.await_echo(p::OP_CALIBRATION_FINISH, Duration::from_millis(600));
        Ok(frame)
    }
}

impl Transport for HidrawTransport {
    /// Ouvre `/dev/hidrawN` en R/W non bloquant.
    fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(HidrawTransport { file })
    }

    /// Libère le descripteur (RAII : `File` se ferme au drop).
    fn close(self) {}

    fn read_cfg(&mut self, device: u8, family: u8, offset: u32) -> Option<[u8; 4]> {
        self.send(&p::build_frame(
            device,
            family,
            p::OP_READ_CFG_DATA,
            &p::offset_bytes(offset),
        ))
        .ok()?;
        let resp = self.await_echo(p::OP_READ_CFG_DATA, READ_TIMEOUT)?;
        if resp.len() < 14 {
            return None;
        }
        let roff = resp[7] as u32 | (resp[8] as u32) << 8 | (resp[9] as u32) << 16;
        if roff != offset {
            return None;
        }
        Some([resp[10], resp[11], resp[12], resp[13]])
    }

    fn request(&mut self, device: u8, family: u8, opcode: u8) -> Option<Vec<u8>> {
        // Réservé aux REQUEST_DEVICE_* (HW/FW/SN/MODE), cf. contrat §1.1.
        if !matches!(
            opcode,
            p::OP_REQUEST_DEVICE_HW
                | p::OP_REQUEST_DEVICE_FW
                | p::OP_REQUEST_DEVICE_SN
                | p::OP_REQUEST_DEVICE_MODE
        ) {
            return None;
        }
        self.send(&p::build_frame(device, family, opcode, &[])).ok()?;
        let resp = self.await_echo(opcode, READ_TIMEOUT)?;
        if resp.len() < 6 {
            return Some(Vec::new());
        }
        // [5]=len (opcode inclus) ; charge utile = [7 : 6+len]
        let length = resp[5] as usize;
        if length > 1 {
            let end = (6 + length).min(resp.len());
            Some(resp[7..end].to_vec())
        } else {
            Some(Vec::new())
        }
    }

    fn write_cfg(
        &mut self,
        device: u8,
        family: u8,
        offset: u32,
        data: [u8; 4],
        allow_identity: bool,
    ) -> Result<(p::Frame, Option<Vec<u8>>), TransportError> {
        // Garde-fou EN DUR, avant toute émission (règle de sécurité n°1, §2.4).
        p::guard_write(offset, allow_identity)?;
        let mut args = p::offset_bytes(offset).to_vec();
        args.extend_from_slice(&data);
        let frame = p::build_frame(device, family, p::OP_WRITE_CFG_DATA, &args);
        self.send(&frame)?;
        let echo = self.await_echo(p::OP_WRITE_CFG_DATA, READ_TIMEOUT);
        Ok((frame, echo))
    }

    fn restart(&mut self, device: u8, family: u8) -> (p::Frame, Option<Vec<u8>>) {
        let frame = p::build_frame(device, family, p::OP_DEVICE_RESTART, &[]);
        let _ = self.send(&frame);
        let echo = self.await_echo(p::OP_DEVICE_RESTART, RESTART_TIMEOUT);
        (frame, echo)
    }
}

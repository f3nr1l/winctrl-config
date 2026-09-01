//! Réaffectation de boutons et répartition 4×32 — couche uinput.
//!
//! La réaffectation et la répartition ne sont pas écrites dans le manche : elles
//! sont réalisées côté OS. On crée un (ou plusieurs) périphérique(s) d'entrée
//! virtuel(s), on capture le manche physique de façon exclusive (`EVIOCGRAB` — les
//! jeux ne le voient plus directement) et on ré-émet les événements, remappés, sur
//! le(s) périphérique(s) virtuel(s).
//!
//! Deux usages :
//! - **Réaffectation** : un périphérique virtuel, boutons réaffectés.
//! - **Répartition 4×32** : plusieurs périphériques virtuels de 32 boutons chacun,
//!   pour offrir tous les boutons du manche aux jeux qui ne gèrent que 32 boutons
//!   par périphérique. Contournement de la limite de 32 boutons, sans toucher au
//!   firmware.
//!
//! **Construction du plan pure** (`plan_split`/`plan_remap`) : testable sans
//! matériel ni `/dev/uinput`. Seuls [`UInputDevice`] et [`RemapSession`] touchent le
//! noyau.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::os::fd::RawFd;

use nix::libc;

use crate::axis_curve::{self, CurveData};
use crate::livemon::{
    eviocgname, parse_events, LiveInput, BTN_JOYSTICK, EV_ABS, EV_KEY, EV_SIZE, EV_SYN,
};

const SYN_REPORT: u16 = 0;

// --- ioctls uinput (linux/uinput.h) ---------------------------------------
const fn ioc(dir: u64, typ: u64, nr: u64, size: u64) -> u64 {
    (dir << 30) | (size << 16) | (typ << 8) | nr
}
const U: u64 = b'U' as u64;
const E: u64 = b'E' as u64;
const IOC_NONE: u64 = 0;
const IOC_WRITE: u64 = 1;

const UI_DEV_CREATE: u64 = ioc(IOC_NONE, U, 1, 0);
const UI_DEV_DESTROY: u64 = ioc(IOC_NONE, U, 2, 0);
const UI_SET_EVBIT: u64 = ioc(IOC_WRITE, U, 100, 4);
const UI_SET_KEYBIT: u64 = ioc(IOC_WRITE, U, 101, 4);
const UI_SET_ABSBIT: u64 = ioc(IOC_WRITE, U, 103, 4);

/// `EVIOCGRAB(int)` sur le nœud evdev physique : capture exclusive des événements.
const EVIOCGRAB: u64 = ioc(IOC_WRITE, E, 0x90, 4);

// Identité des devices virtuels : VID WinWing (reconnaissable) mais **PID hors
// plage réelle** (les manches sont BC29/BC2A) pour que la découverte ne confonde
// jamais un virtuel avec le vrai. bustype = BUS_VIRTUAL.
const BUS_VIRTUAL: u16 = 0x06;
const VIRT_VENDOR: u16 = 0x4098;
pub const VIRT_PRODUCT_BASE: u16 = 0xBCF0;
const UINPUT_PATH: &str = "/dev/uinput";
const UINPUT_MAX_NAME_SIZE: usize = 80;
const ABS_CNT: usize = 64;

/// L'URSA MINOR a ~48 boutons **réels** mais le descripteur HID en déclare ~111.
/// 2 manettes de 32 = 64 boutons couvrent tous les réels — inutile d'en créer 4.
pub const DEFAULT_SPLIT_DEVICES: usize = 2;
pub const DEFAULT_SPLIT_BUTTONS: usize = 32;

// Plage BTN_GAMEPAD (0x130-0x13F) à **éviter** : SDL y voit une manette et masque
// les boutons dans les jeux et les testeurs de manette. Le manche physique l'évite
// aussi (0x120-0x12F puis 0x2C0+). BTN_TRIGGER_HAPPY = 0x2C0, 64 codes en plus.
const BTN_TRIGGER_HAPPY: u16 = 0x2C0;
const JOY_SAFE_COUNT: usize = 0x130 - BTN_JOYSTICK as usize; // 16 : BTN_JOYSTICK..BTN_DEAD
const TRIGGER_HAPPY_COUNT: usize = 64; // 0x2C0..0x2FF

/// Ordinal de sortie 1-based maximal sans collision : 16 codes joystick sûrs + 64
/// BTN_TRIGGER_HAPPY = 80. Borne haute de la table de remap (UI).
pub const MAX_OUTPUT_ORDINAL: usize = JOY_SAFE_COUNT + TRIGGER_HAPPY_COUNT; // 80

/// Code EV_KEY de sortie du `index`-ième bouton virtuel (0-based), en évitant la
/// plage BTN_GAMEPAD. 0-15 -> `BTN_JOYSTICK` ; 16+ -> `BTN_TRIGGER_HAPPY`.
pub fn out_button_code(index: usize) -> u16 {
    if index < JOY_SAFE_COUNT {
        BTN_JOYSTICK + index as u16
    } else {
        BTN_TRIGGER_HAPPY + (index - JOY_SAFE_COUNT) as u16
    }
}

// --- spécification d'un device virtuel (pur) -------------------------------
/// Un axe à déclarer sur un device virtuel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AxisSpec {
    pub code: u16,
    pub minimum: i32,
    pub maximum: i32,
    pub fuzz: i32,
    pub flat: i32,
}

impl AxisSpec {
    pub fn new(code: u16, minimum: i32, maximum: i32) -> Self {
        AxisSpec {
            code,
            minimum,
            maximum,
            fuzz: 0,
            flat: 0,
        }
    }
}

/// Ce qu'un device virtuel expose : un nom, des boutons, des axes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutSpec {
    pub name: String,
    pub buttons: Vec<u16>,
    pub axes: Vec<AxisSpec>,
}

/// Plan complet, **pur** : les devices virtuels à créer + le routage.
///
/// Rotation d'un **couple** d'axes (X/Y, Rx/Ry) : les deux axes `a`/`b` tournent
/// ensemble de `angle_deg` degrés autour de leur centre. Portée par la couche
/// uinput (état inter-axes), pas par `translate` (mono-axe).
#[derive(Debug, Clone, PartialEq)]
pub struct RotationLink {
    pub a: u16,
    pub b: u16,
    pub angle_deg: f64,
    pub a_min: i32,
    pub a_max: i32,
    pub b_min: i32,
    pub b_max: i32,
}

/// `key_route` : code bouton physique -> liste de `(slot, code de sortie)`.
/// `abs_route` : code d'axe physique -> liste de slots (l'axe est recopié).
/// `abs_curve` : code d'axe -> `(courbe, min, max)`, **seulement** pour les axes
/// dont la courbe n'est pas l'identité (inversion/deadzone/courbe/gain/point). L'axe
/// est alors transformé avant ré-émission — cf. [`crate::axis_curve`].
/// `rotations` : code d'axe -> lien de rotation (les deux axes d'un couple pointent
/// le même lien). Appliqué après la courbe, dans la couche uinput.
#[derive(Debug, Clone, PartialEq)]
pub struct RemapPlan {
    pub mode: &'static str, // "split" | "remap" | "curve"
    pub slots: Vec<OutSpec>,
    pub key_route: HashMap<u16, Vec<(usize, u16)>>,
    pub abs_route: HashMap<u16, Vec<usize>>,
    pub abs_curve: HashMap<u16, (CurveData, i32, i32)>,
    pub rotations: HashMap<u16, RotationLink>,
    pub source_buttons: usize,
    pub source_axes: usize,
    pub dropped: usize, // boutons physiques sans sortie
}

impl RemapPlan {
    /// Applique la courbe mono-axe d'un axe (inversion/deadzone/courbe/point), ou
    /// renvoie la valeur brute s'il n'en a pas. La **rotation** (inter-axes) est
    /// gérée à part par [`RemapSession`].
    fn shape_axis(&self, code: u16, value: i32) -> i32 {
        match self.abs_curve.get(&code) {
            Some((curve, min, max)) => curve.apply(value, *min, *max),
            None => value,
        }
    }
}

impl RemapPlan {
    pub fn n_slots(&self) -> usize {
        self.slots.len()
    }

    /// Traduit un événement physique en liste de `(slot, type, code, valeur)`.
    /// Ne s'occupe **pas** de la synchro (EV_SYN) : le démon la propage à tous les
    /// devices. Un événement sans route (bouton en trop du split) -> `[]`.
    fn translate(&self, etype: u16, code: u16, value: i32) -> Vec<(usize, u16, u16, i32)> {
        if etype == EV_KEY {
            if let Some(routes) = self.key_route.get(&code) {
                return routes.iter().map(|&(s, oc)| (s, EV_KEY, oc, value)).collect();
            }
        } else if etype == EV_ABS {
            if let Some(slots) = self.abs_route.get(&code) {
                // Applique la courbe/inversion si l'axe en a une (sinon valeur brute).
                let out = match self.abs_curve.get(&code) {
                    Some((curve, min, max)) => curve.apply(value, *min, *max),
                    None => value,
                };
                return slots.iter().map(|&s| (s, EV_ABS, code, out)).collect();
            }
        }
        Vec::new()
    }
}

/// `code d'axe -> (courbe, min, max)` pour les axes à courbe **non identité**.
/// Les autres n'y figurent pas : [`RemapPlan::translate`] les recopie sans coût.
fn build_abs_curve(
    axes: &[AxisSpec],
    curves: &HashMap<u16, CurveData>,
) -> HashMap<u16, (CurveData, i32, i32)> {
    axes.iter()
        .filter_map(|ax| {
            let c = curves.get(&ax.code).copied()?;
            (!c.is_identity()).then_some((ax.code, (c, ax.minimum, ax.maximum)))
        })
        .collect()
}

/// Couples de rotation : les axes qui partagent un même `rotate_group` (non nul)
/// avec `rotate ≠ 0`. Un groupe doit compter **exactement deux** axes présents
/// (une rotation est une opération de plan) ; sinon il est ignoré. Les deux
/// membres pointent le même [`RotationLink`].
fn build_rotations(
    axes: &[AxisSpec],
    curves: &HashMap<u16, CurveData>,
) -> HashMap<u16, RotationLink> {
    let bounds: HashMap<u16, (i32, i32)> =
        axes.iter().map(|a| (a.code, (a.minimum, a.maximum))).collect();
    let mut groups: BTreeMap<u32, Vec<u16>> = BTreeMap::new();
    for ax in axes {
        if let Some(c) = curves.get(&ax.code) {
            if c.has_rotation() {
                groups.entry(c.rotate_group).or_default().push(ax.code);
            }
        }
    }
    let mut out = HashMap::new();
    for (_group, mut codes) in groups {
        if codes.len() != 2 {
            continue;
        }
        codes.sort_unstable();
        let (a, b) = (codes[0], codes[1]);
        let (a_min, a_max) = bounds[&a];
        let (b_min, b_max) = bounds[&b];
        // a et b portent le même angle (posé ensemble par l'UI) ; on prend celui de a.
        let angle_deg = f64::from(curves[&a].rotate.clamp(-25, 25));
        let link = RotationLink {
            a,
            b,
            angle_deg,
            a_min,
            a_max,
            b_min,
            b_max,
        };
        out.insert(a, link.clone());
        out.insert(b, link);
    }
    out
}

/// Milieu (centre) d'une plage d'axe, valeur par défaut d'un partenaire de rotation
/// pas encore vu.
fn midpoint(min: i32, max: i32) -> i32 {
    min + (max - min) / 2
}

// --- construction des plans (purs) -----------------------------------------
/// Répartit les boutons physiques sur `n_devices` devices de `per_device` boutons.
/// Le bouton physique d'ordre `i` (0-based) sort sur le device `i/per`, bouton local
/// `i%per`. Les axes sont recopiés sur le **seul** device 1 (les autres = bancs de
/// boutons). Boutons au-delà de la capacité : ignorés.
pub fn plan_split(
    buttons: &[u16],
    axes: &[AxisSpec],
    curves: &HashMap<u16, CurveData>,
    n_devices: usize,
    per_device: usize,
    name: &str,
) -> RemapPlan {
    let capacity = n_devices * per_device;
    let mut key_route: HashMap<u16, Vec<(usize, u16)>> = HashMap::new();
    let mut counts = vec![0usize; n_devices];
    let mut dropped = 0;
    for (i, &code) in buttons.iter().enumerate() {
        if i >= capacity {
            dropped += 1;
            continue;
        }
        let slot = i / per_device;
        let local = i % per_device;
        key_route.insert(code, vec![(slot, out_button_code(local))]);
        counts[slot] += 1;
    }
    // Axes + POV sur le SEUL device 1 : il porte le « stick complet ».
    let abs_route: HashMap<u16, Vec<usize>> = axes.iter().map(|ax| (ax.code, vec![0])).collect();
    let slots = (0..n_devices)
        .map(|s| OutSpec {
            name: format!("{name} ({}/{n_devices})", s + 1),
            buttons: (0..counts[s]).map(out_button_code).collect(),
            axes: if s == 0 { axes.to_vec() } else { Vec::new() },
        })
        .collect();
    RemapPlan {
        mode: "split",
        slots,
        key_route,
        abs_route,
        abs_curve: build_abs_curve(axes, curves),
        rotations: build_rotations(axes, curves),
        source_buttons: buttons.len(),
        source_axes: axes.len(),
        dropped,
    }
}

/// Un seul device virtuel. Par défaut chaque bouton physique d'ordre `i` sort à
/// l'identique. `overrides` réaffecte par **ordinal 1-based** (`{1: 5}` = le bouton
/// physique n°1 sort comme bouton virtuel n°5). Les axes passent tels quels.
///
/// Sortie bornée à `MAX_OUTPUT_ORDINAL` (80) : au-delà, `out_button_code`
/// produirait un code > `KEY_MAX` que uinput refuse. Les boutons dont la sortie
/// **identité** dépasserait cette borne (slots fantômes de l'URSA) ne sont pas
/// exposés. Le device déclare des boutons **contigus** 1..max.
pub fn plan_remap(
    buttons: &[u16],
    axes: &[AxisSpec],
    curves: &HashMap<u16, CurveData>,
    overrides: &HashMap<u32, u32>,
    name: &str,
) -> Result<RemapPlan, String> {
    let n = buttons.len() as u32;
    for (&src, &dst) in overrides {
        if !(1..=n).contains(&src) || !(1..=MAX_OUTPUT_ORDINAL as u32).contains(&dst) {
            return Err(format!(
                "remap invalide : {src}->{dst} (source 1..{n}, sortie 1..{MAX_OUTPUT_ORDINAL})"
            ));
        }
    }
    let mut key_route: HashMap<u16, Vec<(usize, u16)>> = HashMap::new();
    let mut out_ordinals: Vec<u32> = Vec::new();
    let mut dropped = 0;
    for (i, &code) in buttons.iter().enumerate() {
        let ordinal = i as u32 + 1;
        let dst = *overrides.get(&ordinal).unwrap_or(&ordinal); // ordinal de sortie 1-based
        if dst as usize > MAX_OUTPUT_ORDINAL {
            dropped += 1; // sortie identité hors plage sûre (bouton haut fantôme)
            continue;
        }
        key_route.insert(code, vec![(0, out_button_code(dst as usize - 1))]);
        out_ordinals.push(dst);
    }
    let max_ord = out_ordinals.iter().copied().max().unwrap_or(0) as usize;
    let out_buttons: Vec<u16> = (0..max_ord).map(out_button_code).collect();
    let abs_route: HashMap<u16, Vec<usize>> = axes.iter().map(|ax| (ax.code, vec![0])).collect();
    let slots = vec![OutSpec {
        name: name.to_string(),
        buttons: out_buttons,
        axes: axes.to_vec(),
    }];
    Ok(RemapPlan {
        mode: "remap",
        slots,
        key_route,
        abs_route,
        abs_curve: build_abs_curve(axes, curves),
        rotations: build_rotations(axes, curves),
        source_buttons: buttons.len(),
        source_axes: axes.len(),
        dropped,
    })
}

/// Convertit les axes découverts par le moniteur live en `AxisSpec`.
pub fn axes_from_live(li: &LiveInput) -> Vec<AxisSpec> {
    li.state
        .axes
        .iter()
        .map(|a| AxisSpec::new(a.code, a.minimum, a.maximum))
        .collect()
}

/// Nom du device physique (`EVIOCGNAME`), pour étiqueter les manettes virtuelles.
pub fn source_name(li: &LiveInput) -> String {
    let maxlen = 128usize;
    let mut buf = vec![0u8; maxlen];
    // SAFETY : fd valide, buf assez grand pour la request EVIOCGNAME(maxlen).
    let rc = unsafe {
        libc::ioctl(
            li.fileno(),
            eviocgname(maxlen as u64) as libc::c_ulong,
            buf.as_mut_ptr(),
        )
    };
    if rc < 0 {
        return String::new();
    }
    let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

fn side_suffix(source: &str) -> &'static str {
    let s = source.trim_end();
    if s.ends_with('R') {
        " R"
    } else if s.ends_with('L') {
        " L"
    } else {
        ""
    }
}

/// « WinWing URSA MINOR R/L » déduit du device physique, pour que deux splits
/// simultanés (L et R) donnent des manettes virtuelles distinctes.
pub fn virtual_base_name(li: &LiveInput) -> String {
    format!("WinWing URSA MINOR{}", side_suffix(&source_name(li)))
}

/// Construit le plan à partir d'un manche déjà ouvert. `name` déduit du côté L/R.
pub fn build_plan(
    li: &LiveInput,
    mode: &str,
    overrides: &HashMap<u32, u32>,
    curves: &HashMap<u16, CurveData>,
) -> Result<RemapPlan, String> {
    let name = virtual_base_name(li);
    let axes = axes_from_live(li);
    match mode {
        "split" => Ok(plan_split(
            &li.state.buttons,
            &axes,
            curves,
            DEFAULT_SPLIT_DEVICES,
            DEFAULT_SPLIT_BUTTONS,
            &name,
        )),
        "remap" => plan_remap(
            &li.state.buttons,
            &axes,
            curves,
            overrides,
            &format!("{name} (remap)"),
        ),
        // Courbe/inversion seule : remap **identité** (aucun override) + courbes.
        // Un device virtuel qui recopie boutons et axes, ces derniers transformés.
        "curve" => plan_remap(
            &li.state.buttons,
            &axes,
            curves,
            &HashMap::new(),
            &format!("{name} (courbe)"),
        ),
        _ => Err(format!("mode inconnu : {mode}")),
    }
}

// --- device virtuel uinput (impur : touche le noyau) -----------------------
/// Sérialise `struct uinput_user_dev` (linux/uinput.h) : 1116 octets sur LP64.
fn pack_user_dev(name: &str, product: u16, version: u16, axes: &[AxisSpec]) -> Vec<u8> {
    let mut absmin = [0i32; ABS_CNT];
    let mut absmax = [0i32; ABS_CNT];
    let mut absfuzz = [0i32; ABS_CNT];
    let mut absflat = [0i32; ABS_CNT];
    for ax in axes {
        let c = ax.code as usize;
        if c < ABS_CNT {
            absmin[c] = ax.minimum;
            absmax[c] = ax.maximum;
            absfuzz[c] = ax.fuzz;
            absflat[c] = ax.flat;
        }
    }
    let mut buf = Vec::with_capacity(1116);
    // name[80] (UINPUT_MAX_NAME_SIZE), tronqué + rempli de zéros.
    let name_b = name.as_bytes();
    let take = name_b.len().min(UINPUT_MAX_NAME_SIZE - 1);
    buf.extend_from_slice(&name_b[..take]);
    buf.resize(UINPUT_MAX_NAME_SIZE, 0);
    // struct input_id { bustype, vendor, product, version } (4×u16).
    buf.extend_from_slice(&BUS_VIRTUAL.to_ne_bytes());
    buf.extend_from_slice(&VIRT_VENDOR.to_ne_bytes());
    buf.extend_from_slice(&product.to_ne_bytes());
    buf.extend_from_slice(&version.to_ne_bytes());
    buf.extend_from_slice(&0u32.to_ne_bytes()); // ff_effects_max
    for arr in [&absmax, &absmin, &absfuzz, &absflat] {
        for v in arr {
            buf.extend_from_slice(&v.to_ne_bytes());
        }
    }
    debug_assert_eq!(buf.len(), 1116);
    buf
}

/// Un `ioctl` uinput passant un entier **par valeur** (UI_SET_*, EVIOCGRAB…).
fn ioctl_val(fd: RawFd, request: u64, value: libc::c_int) -> io::Result<()> {
    // SAFETY : fd valide, request = ioctl acceptant un int par valeur.
    let rc = unsafe { libc::ioctl(fd, request as libc::c_ulong, value) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Un device d'entrée virtuel créé via `/dev/uinput`.
pub struct UInputDevice {
    fd: RawFd,
}

impl UInputDevice {
    /// Déclare les capacités puis `UI_DEV_CREATE`. Erreur si `/dev/uinput`
    /// inaccessible (règle udev/71) ou module `uinput` non chargé.
    pub fn create(spec: &OutSpec, product: u16, version: u16) -> io::Result<Self> {
        let cpath = std::ffi::CString::new(UINPUT_PATH).unwrap();
        // SAFETY : chemin constant valide.
        let fd = unsafe { libc::open(cpath.as_ptr(), libc::O_WRONLY | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let dev = UInputDevice { fd };
        dev.setup(spec, product, version)?; // en cas d'échec, Drop ferme le fd
        Ok(dev)
    }

    fn setup(&self, spec: &OutSpec, product: u16, version: u16) -> io::Result<()> {
        ioctl_val(self.fd, UI_SET_EVBIT, EV_SYN as libc::c_int)?;
        if !spec.buttons.is_empty() {
            ioctl_val(self.fd, UI_SET_EVBIT, EV_KEY as libc::c_int)?;
            for &code in &spec.buttons {
                ioctl_val(self.fd, UI_SET_KEYBIT, code as libc::c_int)?;
            }
        }
        if !spec.axes.is_empty() {
            ioctl_val(self.fd, UI_SET_EVBIT, EV_ABS as libc::c_int)?;
            for ax in &spec.axes {
                ioctl_val(self.fd, UI_SET_ABSBIT, ax.code as libc::c_int)?;
            }
        }
        let payload = pack_user_dev(&spec.name, product, version, &spec.axes);
        // SAFETY : fd valide, payload de 1116 octets.
        let n = unsafe {
            libc::write(
                self.fd,
                payload.as_ptr() as *const libc::c_void,
                payload.len(),
            )
        };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY : fd valide, UI_DEV_CREATE sans argument.
        let rc = unsafe { libc::ioctl(self.fd, UI_DEV_CREATE as libc::c_ulong) };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn emit(&self, etype: u16, code: u16, value: i32) {
        let mut ev = [0u8; EV_SIZE];
        ev[16..18].copy_from_slice(&etype.to_ne_bytes());
        ev[18..20].copy_from_slice(&code.to_ne_bytes());
        ev[20..24].copy_from_slice(&value.to_ne_bytes());
        // SAFETY : fd valide, ev de EV_SIZE octets.
        unsafe {
            libc::write(self.fd, ev.as_ptr() as *const libc::c_void, ev.len());
        }
    }

    fn syn(&self) {
        self.emit(EV_SYN, SYN_REPORT, 0);
    }
}

impl Drop for UInputDevice {
    fn drop(&mut self) {
        if self.fd >= 0 {
            // SAFETY : fd ouvert par nous ; UI_DEV_DESTROY best-effort puis close.
            unsafe {
                libc::ioctl(self.fd, UI_DEV_DESTROY as libc::c_ulong);
                libc::close(self.fd);
            }
            self.fd = -1;
        }
    }
}

/// Session de remap active : manche physique *grabbé* + devices virtuels.
///
/// **Possède** le [`LiveInput`] source (créé au `start`, fermé au `Drop`), à la
/// différence du Python où la session ne le possède pas — ici c'est plus sûr côté
/// durée de vie. `start` (grab + création), `pump` (draine non bloquant), `Drop`
/// (destroy + ungrab).
pub struct RemapSession {
    li: LiveInput,
    plan: RemapPlan,
    devices: Vec<UInputDevice>,
    grabbed: bool,
    /// Dernière valeur **façonnée** (post-courbe) de chaque axe, pour la rotation
    /// de couple : quand un axe tourne, il faut la valeur courante de son partenaire.
    axis_shaped: HashMap<u16, i32>,
}

impl RemapSession {
    pub fn new(li: LiveInput, plan: RemapPlan) -> Self {
        RemapSession {
            li,
            plan,
            devices: Vec::new(),
            grabbed: false,
            axis_shaped: HashMap::new(),
        }
    }

    pub fn fileno(&self) -> RawFd {
        self.li.fileno()
    }

    pub fn n_slots(&self) -> usize {
        self.plan.n_slots()
    }

    /// Grab le manche (capture exclusive) puis crée les devices virtuels. En cas
    /// d'échec de création (uinput indispo), défait le grab avant de propager.
    pub fn start(&mut self) -> io::Result<()> {
        ioctl_val(self.li.fileno(), EVIOCGRAB, 1)?;
        self.grabbed = true;
        for (i, spec) in self.plan.slots.iter().enumerate() {
            match UInputDevice::create(spec, VIRT_PRODUCT_BASE + i as u16, i as u16) {
                Ok(d) => self.devices.push(d),
                Err(e) => {
                    self.teardown();
                    return Err(e);
                }
            }
        }
        Ok(())
    }

    /// Draine les événements en attente (non bloquant). Nombre traité. Erreur si le
    /// manche disparaît (à traiter par l'appelant).
    pub fn pump(&mut self) -> io::Result<usize> {
        let mut total = 0;
        let mut buf = [0u8; EV_SIZE * 64];
        loop {
            // SAFETY : fd valide, buf assez grand.
            let n = unsafe {
                libc::read(
                    self.li.fileno(),
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    break;
                }
                return Err(err); // manche déconnecté (ENODEV…)
            }
            if n == 0 {
                break;
            }
            let n = n as usize;
            total += self.route(&buf[..n]);
            if n < buf.len() {
                break;
            }
        }
        Ok(total)
    }

    /// Traduit un tampon d'événements bruts et l'émet sur les devices virtuels. La
    /// synchro (EV_SYN) est propagée à **tous** les devices. L'auto-répétition
    /// EV_KEY (value == 2) est filtrée (sinon boutons « collés »). Les axes passent
    /// par [`Self::route_abs`] (courbe + rotation de couple, avec état).
    fn route(&mut self, buf: &[u8]) -> usize {
        let mut n = 0;
        for (etype, code, value) in parse_events(buf) {
            if etype == EV_SYN {
                for d in &self.devices {
                    d.syn();
                }
            } else if etype == EV_KEY && value == 2 {
                // auto-répétition : ni utile ni souhaitable (appui fantôme collé).
            } else if etype == EV_ABS {
                self.route_abs(code, value);
            } else {
                for (slot, ot, oc, ov) in self.plan.translate(etype, code, value) {
                    if let Some(d) = self.devices.get(slot) {
                        d.emit(ot, oc, ov);
                    }
                }
            }
            n += 1;
        }
        n
    }

    /// Route un axe : applique sa courbe mono-axe, puis — s'il fait partie d'un
    /// couple de rotation — tourne la paire avec la dernière valeur du partenaire et
    /// ré-émet **les deux** axes. Sinon ré-émet le seul axe façonné.
    fn route_abs(&mut self, code: u16, value: i32) {
        let shaped = self.plan.shape_axis(code, value);
        if let Some(link) = self.plan.rotations.get(&code).cloned() {
            self.axis_shaped.insert(code, shaped);
            let a_val = self
                .axis_shaped
                .get(&link.a)
                .copied()
                .unwrap_or_else(|| midpoint(link.a_min, link.a_max));
            let b_val = self
                .axis_shaped
                .get(&link.b)
                .copied()
                .unwrap_or_else(|| midpoint(link.b_min, link.b_max));
            let (oa, ob) = axis_curve::rotate_pair(
                a_val, link.a_min, link.a_max, b_val, link.b_min, link.b_max, link.angle_deg,
            );
            self.emit_abs(link.a, oa);
            self.emit_abs(link.b, ob);
        } else {
            self.emit_abs(code, shaped);
        }
    }

    /// Émet une valeur d'axe sur tous les slots qui le portent.
    fn emit_abs(&self, code: u16, value: i32) {
        if let Some(slots) = self.plan.abs_route.get(&code) {
            for &slot in slots {
                if let Some(d) = self.devices.get(slot) {
                    d.emit(EV_ABS, code, value);
                }
            }
        }
    }

    fn teardown(&mut self) {
        self.devices.clear(); // Drop de chaque UInputDevice : UI_DEV_DESTROY + close
        if self.grabbed {
            let _ = ioctl_val(self.li.fileno(), EVIOCGRAB, 0);
            self.grabbed = false;
        }
    }
}

impl Drop for RemapSession {
    fn drop(&mut self) {
        self.teardown();
    }
}

/// Décrit un plan sans rien créer (aperçu). Utile en CLI et pour l'UI.
pub fn preview(plan: &RemapPlan, node: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    if plan.mode == "split" {
        lines.push(format!(
            "Split {}×{} (contournement de la limite 32 boutons)",
            plan.n_slots(),
            DEFAULT_SPLIT_BUTTONS
        ));
    } else {
        lines.push("Remap de boutons (1 périphérique virtuel)".to_string());
    }
    lines.push(format!(
        "  source : {node} — {} boutons, {} axes",
        plan.source_buttons, plan.source_axes
    ));
    lines.push(format!(
        "  → {} périphérique(s) virtuel(s) :",
        plan.n_slots()
    ));
    for (i, spec) in plan.slots.iter().enumerate() {
        let nb = spec.buttons.len();
        let span = if plan.mode == "split" && nb > 0 {
            let base = i * DEFAULT_SPLIT_BUTTONS;
            format!("boutons {}–{}", base + 1, base + nb)
        } else {
            format!("{nb} boutons")
        };
        lines.push(format!(
            "     • {} : {span}  (+ {} axes)",
            spec.name,
            spec.axes.len()
        ));
    }
    if plan.dropped > 0 {
        lines.push(format!(
            "  ({} boutons déclarés au-delà de la capacité, non exposés — souvent des \
             slots fantômes du firmware).",
            plan.dropped
        ));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buttons(n: usize) -> Vec<u16> {
        (0..n as u16).map(|i| BTN_JOYSTICK + i).collect()
    }

    #[test]
    fn out_codes_avoid_gamepad_range() {
        assert_eq!(out_button_code(0), 0x120); // BTN_JOYSTICK
        assert_eq!(out_button_code(15), 0x12F); // dernier code sûr
        assert_eq!(out_button_code(16), 0x2C0); // saute BTN_GAMEPAD -> BTN_TRIGGER_HAPPY
        assert_eq!(out_button_code(MAX_OUTPUT_ORDINAL - 1), 0x2FF); // KEY_MAX
        assert!(out_button_code(MAX_OUTPUT_ORDINAL - 1) <= crate::livemon::KEY_MAX);
    }

    fn no_curves() -> HashMap<u16, CurveData> {
        HashMap::new()
    }

    #[test]
    fn split_distributes_and_axes_on_first_only() {
        let axes = vec![AxisSpec::new(0, 0, 65535)];
        let plan = plan_split(&buttons(40), &axes, &no_curves(), 2, 32, "URSA");
        assert_eq!(plan.n_slots(), 2);
        assert_eq!(plan.slots[0].buttons.len(), 32);
        assert_eq!(plan.slots[1].buttons.len(), 8);
        assert!(!plan.slots[0].axes.is_empty());
        assert!(plan.slots[1].axes.is_empty()); // axes/POV device 1 seulement
        // bouton physique 33 (index 32) -> slot 1, bouton local 0.
        assert_eq!(plan.key_route[&(BTN_JOYSTICK + 32)], vec![(1, 0x120)]);
    }

    #[test]
    fn split_drops_beyond_capacity() {
        let plan = plan_split(&buttons(70), &[], &no_curves(), 2, 32, "URSA");
        assert_eq!(plan.dropped, 70 - 64);
    }

    #[test]
    fn remap_identity_by_default() {
        let plan = plan_remap(&buttons(3), &[], &no_curves(), &HashMap::new(), "r").unwrap();
        assert_eq!(plan.key_route[&BTN_JOYSTICK], vec![(0, out_button_code(0))]);
        assert_eq!(plan.slots[0].buttons.len(), 3);
    }

    #[test]
    fn remap_override_applies() {
        let mut ov = HashMap::new();
        ov.insert(1u32, 5u32); // bouton n°1 -> sortie n°5
        let plan = plan_remap(&buttons(3), &[], &no_curves(), &ov, "r").unwrap();
        assert_eq!(plan.key_route[&BTN_JOYSTICK], vec![(0, out_button_code(4))]);
        assert_eq!(plan.slots[0].buttons.len(), 5); // contigus 1..5
    }

    #[test]
    fn remap_rejects_out_of_range() {
        let mut ov = HashMap::new();
        ov.insert(1u32, 999u32);
        assert!(plan_remap(&buttons(3), &[], &no_curves(), &ov, "r").is_err());
        ov.clear();
        ov.insert(9u32, 2u32); // source au-delà des boutons
        assert!(plan_remap(&buttons(3), &[], &no_curves(), &ov, "r").is_err());
    }

    #[test]
    fn remap_drops_phantom_high_buttons() {
        // 82 boutons : les identités 81 et 82 dépassent MAX_OUTPUT_ORDINAL (80).
        let plan = plan_remap(&buttons(82), &[], &no_curves(), &HashMap::new(), "r").unwrap();
        assert_eq!(plan.dropped, 2);
        assert_eq!(plan.slots[0].buttons.len(), MAX_OUTPUT_ORDINAL);
    }

    #[test]
    fn translate_filters_unknown() {
        let plan = plan_split(&buttons(2), &[], &no_curves(), 2, 32, "URSA");
        assert!(plan.translate(EV_KEY, 0x999, 1).is_empty());
        assert_eq!(
            plan.translate(EV_KEY, BTN_JOYSTICK, 1),
            vec![(0, EV_KEY, 0x120, 1)]
        );
    }

    #[test]
    fn translate_passes_axis_through_without_curve() {
        let axes = vec![AxisSpec::new(crate::livemon::ABS_THROTTLE, 0, 4095)];
        let plan = plan_split(&buttons(2), &axes, &no_curves(), 2, 32, "URSA");
        assert!(plan.abs_curve.is_empty());
        assert_eq!(
            plan.translate(EV_ABS, crate::livemon::ABS_THROTTLE, 1000),
            vec![(0, EV_ABS, crate::livemon::ABS_THROTTLE, 1000)]
        );
    }

    #[test]
    fn translate_applies_reverse_curve_to_axis() {
        let code = crate::livemon::ABS_THROTTLE;
        let axes = vec![AxisSpec::new(code, 0, 4095)];
        let mut curves = HashMap::new();
        curves.insert(
            code,
            CurveData {
                is_reversed: true,
                ..Default::default()
            },
        );
        let plan = plan_split(&buttons(2), &axes, &curves, 2, 32, "URSA");
        assert_eq!(plan.abs_curve.len(), 1);
        // inversion : 0 -> 4095, 4095 -> 0.
        assert_eq!(plan.translate(EV_ABS, code, 0), vec![(0, EV_ABS, code, 4095)]);
        assert_eq!(plan.translate(EV_ABS, code, 4095), vec![(0, EV_ABS, code, 0)]);
    }

    #[test]
    fn rotation_group_pairs_two_axes() {
        let (x, y) = (crate::livemon::ABS_X, crate::livemon::ABS_Y);
        let axes = vec![AxisSpec::new(x, -100, 100), AxisSpec::new(y, -100, 100)];
        let rot = CurveData {
            rotate: 15,
            rotate_group: 1,
            ..Default::default()
        };
        let mut curves = HashMap::new();
        curves.insert(x, rot);
        curves.insert(y, rot);
        let plan = plan_split(&buttons(2), &axes, &curves, 2, 32, "URSA");
        assert_eq!(plan.rotations.len(), 2);
        let link = &plan.rotations[&x];
        assert_eq!((link.a, link.b), (x, y));
        assert_eq!(link.angle_deg, 15.0);
        // rotation seule (pas de courbe) : abs_curve reste vide.
        assert!(plan.abs_curve.is_empty());
    }

    #[test]
    fn lone_rotation_axis_makes_no_group() {
        let x = crate::livemon::ABS_X;
        let axes = vec![AxisSpec::new(x, -100, 100)];
        let mut curves = HashMap::new();
        curves.insert(
            x,
            CurveData {
                rotate: 15,
                rotate_group: 1,
                ..Default::default()
            },
        );
        let plan = plan_split(&buttons(2), &axes, &curves, 2, 32, "URSA");
        assert!(plan.rotations.is_empty(), "un couple exige 2 axes");
    }

    #[test]
    fn identity_curve_is_not_stored() {
        let code = crate::livemon::ABS_X;
        let axes = vec![AxisSpec::new(code, 0, 65535)];
        let mut curves = HashMap::new();
        curves.insert(code, CurveData::default()); // identité
        let plan = plan_remap(&buttons(2), &axes, &curves, &HashMap::new(), "r").unwrap();
        assert!(plan.abs_curve.is_empty(), "l'identité ne doit pas être routée");
    }

    #[test]
    fn pack_user_dev_is_1116_bytes() {
        let axes = vec![AxisSpec::new(0, 0, 65535)];
        assert_eq!(pack_user_dev("t", 0xBCF0, 0, &axes).len(), 1116);
    }
}

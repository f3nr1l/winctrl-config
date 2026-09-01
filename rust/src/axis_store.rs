//! Persistance des courbes/inversions d'axe (D-8) — sans GTK.
//!
//! Une config d'axe = un [`crate::axis_curve::CurveData`] par **code d'axe**
//! (`ABS_THROTTLE`, `ABS_X`…). On la stocke **par appareil** en JSON sous
//! `~/.local/share/winctrl/axis/`, exactement comme [`crate::remap_store`] pour les
//! remaps de boutons (mêmes clé d'appareil, slug, restitution sous sudo).
//!
//! Le modèle ([`AxisCurves`]) et la (dé)sérialisation sont **purs et testables** ;
//! seules [`load_curves`]/[`save_curves`] touchent le disque.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::axis_curve::{CurveData, CurveType};
use crate::enumerate::WinwingDevice;
use crate::remap_store::{chown_to_user, device_key, slug, user_home};

const AXIS_FORMAT: &str = "winwing-axis";
const AXIS_VERSION: u64 = 1;
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Courbes d'axe d'un appareil : `code d'axe -> CurveData`. Seules les courbes
/// **non identité** sont conservées (poser une identité **retire** l'entrée : le
/// plan recopie déjà l'axe tel quel).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AxisCurves {
    curves: BTreeMap<u16, CurveData>,
}

impl AxisCurves {
    pub fn new() -> Self {
        Self::default()
    }

    /// Enregistre la courbe de l'axe `code`. Une courbe **sans effet** la retire :
    /// identité mono-axe **et** sans rotation (la rotation n'est pas dans `apply`
    /// mais doit tout de même être persistée).
    pub fn set(&mut self, code: u16, curve: CurveData) {
        if curve.is_identity() && !curve.has_rotation() {
            self.curves.remove(&code);
        } else {
            self.curves.insert(code, curve);
        }
    }

    /// La courbe de l'axe `code`, ou l'identité (défaut) si aucune.
    pub fn get(&self, code: u16) -> CurveData {
        self.curves.get(&code).copied().unwrap_or_default()
    }

    pub fn clear(&mut self, code: u16) {
        self.curves.remove(&code);
    }

    pub fn clear_all(&mut self) {
        self.curves.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.curves.is_empty()
    }

    pub fn len(&self) -> usize {
        self.curves.len()
    }

    /// Courbes triées par code d'axe (pour l'affichage).
    pub fn entries(&self) -> impl Iterator<Item = (u16, CurveData)> + '_ {
        self.curves.iter().map(|(&c, &d)| (c, d))
    }

    /// Copie prête pour [`crate::remap::build_plan`] / `plan_*`.
    pub fn to_curves(&self) -> HashMap<u16, CurveData> {
        self.curves.iter().map(|(&c, &d)| (c, d)).collect()
    }

    fn to_json(&self, device_meta: Value) -> Value {
        let axes: serde_json::Map<String, Value> = self
            .curves
            .iter()
            .map(|(&code, c)| (code.to_string(), curve_to_json(c)))
            .collect();
        json!({
            "format": AXIS_FORMAT,
            "version": AXIS_VERSION,
            "app_version": APP_VERSION,
            "device": device_meta,
            "axes": axes,
        })
    }

    fn from_json(obj: &Value) -> Result<Self, String> {
        if obj.get("format").and_then(Value::as_str) != Some(AXIS_FORMAT) {
            return Err("format de courbe d'axe inattendu".into());
        }
        let version = obj.get("version").and_then(Value::as_u64).unwrap_or(0);
        if version > AXIS_VERSION {
            return Err(format!("version de courbe trop récente : {version}"));
        }
        let raw = obj
            .get("axes")
            .and_then(Value::as_object)
            .ok_or("config sans table d'axes")?;
        let mut m = AxisCurves::new();
        for (k, v) in raw {
            let code: u16 = k.parse().map_err(|_| "code d'axe non entier")?;
            m.set(code, curve_from_json(v));
        }
        Ok(m)
    }
}

/// `CurveData` -> JSON (tous les champs, pour rester lisible/diffable à la main).
fn curve_to_json(c: &CurveData) -> Value {
    json!({
        "curve_type": c.curve_type.as_str(),
        "is_reversed": c.is_reversed,
        "lower": c.lower,
        "upper": c.upper,
        "center_lower": c.center_lower,
        "center_upper": c.center_upper,
        "curve": c.curve,
        "zoom": c.zoom,
        "x_pos": c.x_pos,
        "y_pos": c.y_pos,
        "rotate": c.rotate,
        "rotate_group": c.rotate_group,
    })
}

/// JSON -> `CurveData`, tolérant : tout champ absent/mal typé retombe sur le
/// défaut (l'ouverture de la page ne doit jamais échouer pour une config douteuse).
fn curve_from_json(v: &Value) -> CurveData {
    let d = CurveData::default();
    let u8f = |k: &str, def: u8| v.get(k).and_then(Value::as_u64).map(|n| n as u8).unwrap_or(def);
    let i8f = |k: &str, def: i8| v.get(k).and_then(Value::as_i64).map(|n| n as i8).unwrap_or(def);
    CurveData {
        curve_type: v
            .get("curve_type")
            .and_then(Value::as_str)
            .map(CurveType::from_label)
            .unwrap_or(d.curve_type),
        is_reversed: v
            .get("is_reversed")
            .and_then(Value::as_bool)
            .unwrap_or(d.is_reversed),
        lower: u8f("lower", d.lower),
        upper: u8f("upper", d.upper),
        center_lower: u8f("center_lower", d.center_lower),
        center_upper: u8f("center_upper", d.center_upper),
        curve: i8f("curve", d.curve),
        zoom: i8f("zoom", d.zoom),
        x_pos: u8f("x_pos", d.x_pos),
        y_pos: u8f("y_pos", d.y_pos),
        rotate: i8f("rotate", d.rotate),
        rotate_group: v
            .get("rotate_group")
            .and_then(Value::as_u64)
            .map(|n| n as u32)
            .unwrap_or(d.rotate_group),
    }
}

// --- métadonnées / chemin (purs) ------------------------------------------
fn device_meta(dev: &WinwingDevice) -> Value {
    json!({
        "pid": format!("{:04x}", dev.pid),
        "serial": dev.serial,
        "product_name": dev.product,
    })
}

pub fn axis_dir() -> PathBuf {
    user_home().join(".local/share/winctrl/axis")
}

pub fn curves_path(dev: &WinwingDevice, directory: Option<&Path>) -> PathBuf {
    let dir = directory.map(Path::to_path_buf).unwrap_or_else(axis_dir);
    dir.join(format!("{}.json", slug(&device_key(dev))))
}

// --- I/O disque ------------------------------------------------------------
/// Charge les courbes de l'appareil, ou une config **vide** si aucun fichier (ou
/// illisible/corrompu) — l'ouverture de la page ne doit jamais échouer pour ça.
pub fn load_curves(dev: &WinwingDevice, directory: Option<&Path>) -> AxisCurves {
    let path = curves_path(dev, directory);
    match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(v) => AxisCurves::from_json(&v).unwrap_or_default(),
            Err(_) => AxisCurves::new(),
        },
        Err(_) => AxisCurves::new(),
    }
}

/// Écrit les courbes en JSON. Une config **vide** supprime le fichier. Restitue les
/// fichiers à l'utilisateur réel sous sudo (comme `remap_store`).
pub fn save_curves(
    dev: &WinwingDevice,
    curves: &AxisCurves,
    directory: Option<&Path>,
) -> std::io::Result<PathBuf> {
    let dir = directory.map(Path::to_path_buf).unwrap_or_else(axis_dir);
    let path = curves_path(dev, Some(&dir));
    if curves.is_empty() {
        let _ = std::fs::remove_file(&path);
        return Ok(path);
    }
    std::fs::create_dir_all(&dir)?;
    let text = serde_json::to_string_pretty(&curves.to_json(device_meta(dev)))?;
    std::fs::write(&path, text)?;
    if let Some(parent) = dir.parent() {
        chown_to_user(&[parent, &dir, &path]);
    } else {
        chown_to_user(&[&dir, &path]);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enumerate::Controller;
    use crate::livemon::{ABS_THROTTLE, ABS_X};

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

    fn reversed() -> CurveData {
        CurveData {
            is_reversed: true,
            ..Default::default()
        }
    }

    #[test]
    fn identity_curve_is_not_stored() {
        let mut c = AxisCurves::new();
        c.set(ABS_THROTTLE, CurveData::default());
        assert!(c.is_empty());
    }

    #[test]
    fn set_get_roundtrip() {
        let mut c = AxisCurves::new();
        c.set(ABS_THROTTLE, reversed());
        assert_eq!(c.len(), 1);
        assert!(c.get(ABS_THROTTLE).is_reversed);
        assert!(!c.get(ABS_X).is_reversed); // absent -> identité
    }

    #[test]
    fn json_roundtrip_preserves_all_fields() {
        let mut c = AxisCurves::new();
        let full = CurveData {
            curve_type: CurveType::J,
            is_reversed: true,
            lower: 5,
            upper: 7,
            center_lower: 3,
            center_upper: 4,
            curve: -11,
            zoom: 6,
            x_pos: 40,
            y_pos: 60,
            rotate: -12,
            rotate_group: 42,
        };
        c.set(ABS_THROTTLE, full);
        let v = c.to_json(json!({}));
        let back = AxisCurves::from_json(&v).unwrap();
        assert_eq!(back.get(ABS_THROTTLE), full);
    }

    #[test]
    fn from_json_is_tolerant_to_missing_fields() {
        let v = json!({
            "format": AXIS_FORMAT,
            "version": 1,
            "axes": { "6": { "is_reversed": true } }
        });
        let c = AxisCurves::from_json(&v).unwrap();
        let got = c.get(ABS_THROTTLE);
        assert!(got.is_reversed);
        assert_eq!(got.curve_type, CurveType::S); // défaut
        assert_eq!(got.x_pos, 50);
    }

    #[test]
    fn rejects_wrong_format() {
        let v = json!({ "format": "nope", "axes": {} });
        assert!(AxisCurves::from_json(&v).is_err());
    }

    #[test]
    fn save_load_via_disk() {
        let tmp = std::env::temp_dir().join(format!("ww-axis-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let d = dev(0xBC29, "SN1");
        let mut c = AxisCurves::new();
        c.set(ABS_THROTTLE, reversed());
        let p = save_curves(&d, &c, Some(&tmp)).unwrap();
        assert!(p.exists());
        let loaded = load_curves(&d, Some(&tmp));
        assert_eq!(loaded, c);
        // config vidée -> fichier retiré
        let empty = AxisCurves::new();
        save_curves(&d, &empty, Some(&tmp)).unwrap();
        assert!(!p.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn distinct_devices_distinct_files() {
        assert_ne!(
            curves_path(&dev(0xBC29, "A"), None),
            curves_path(&dev(0xBC29, "B"), None)
        );
    }
}

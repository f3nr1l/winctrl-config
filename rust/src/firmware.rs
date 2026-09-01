//! Vérification de la **dernière version de firmware** au catalogue WinWing.
//!
//! **Lecture seule / information.** On lit la version matérielle (HW) et firmware
//! (FW) de chaque contrôleur (`REQUEST_DEVICE_HW`/`FW`, cf. [`crate::model`]), on
//! interroge le catalogue public `winctrl.com` et on rapporte la version
//! disponible. Aucun flash n'est effectué ici.
//!
//! Les fonctions ici sont **pures** (build de requête, parsing) : l'appel réseau
//! (via `curl`) et la lecture device sont faits par l'appelant (page Général), hors
//! thread UI.

use serde_json::{json, Value};

/// `pid` du catalogue = `(family << 8) | device` (ex. grip `0xbf0a`).
pub fn catalog_pid(device: u8, family: u8) -> u32 {
    ((family as u32) << 8) | device as u32
}

/// Version matérielle 16 bits -> chaîne `hardWare` de l'API (`hi.lo`, lo sur 2 hex).
pub fn hardware_string(hwver: u16) -> String {
    format!("{:x}.{:02x}", hwver >> 8, hwver & 0xff)
}

/// Clé d'entrée du catalogue pour un `(pid, hardWare)` : `"<pid>&<hardWare>"`.
pub fn catalog_key(pid: u32, hardware: &str) -> String {
    format!("{pid}&{hardware}")
}

/// Corps JSON de la requête catalogue : un tableau de `{pid, hardWare}`.
pub fn build_query_body(items: &[(u32, String)]) -> String {
    let arr: Vec<Value> = items
        .iter()
        .map(|(pid, hw)| json!({ "pid": pid, "hardWare": hw }))
        .collect();
    Value::Array(arr).to_string()
}

/// Extrait `firmware_version` de l'entrée `data["<pid>&<hardWare>"]` d'une réponse
/// catalogue. `None` si absente/illisible.
pub fn parse_latest_version(response: &str, pid: u32, hardware: &str) -> Option<String> {
    let v: Value = serde_json::from_str(response).ok()?;
    let entry = v.get("data")?.get(catalog_key(pid, hardware))?;
    match entry.get("firmware_version")? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

/// Décode la version firmware depuis la charge utile de `REQUEST_DEVICE_FW`
/// (`crate::model::Transport::request`, opcode retiré) : `[dev, fam, minor, major]`.
/// Rend `"major.minor"` (minor sur 2 chiffres), **au même format que le catalogue**
/// (ex. grip R : octets `02 01` -> `"1.02"`).
pub fn decode_fw_version(payload: &[u8]) -> Option<String> {
    // minor en HEXA (throttle 0x16 -> « 1.16 », comme le nom de fichier/catalogue).
    match (payload.get(2), payload.get(3)) {
        (Some(&minor), Some(&major)) => Some(format!("{major}.{minor:02x}")),
        _ => None,
    }
}

/// Décode la version matérielle depuis la charge utile de `REQUEST_DEVICE_HW`
/// (opcode retiré) : `[type_lo, type_hi, hw_lo, hw_hi]` -> `hwver` u16 LE.
pub fn decode_hw(payload: &[u8]) -> Option<u16> {
    match (payload.get(2), payload.get(3)) {
        (Some(&lo), Some(&hi)) => Some(lo as u16 | ((hi as u16) << 8)),
        _ => None,
    }
}

/// Décode le **type** matériel depuis la même charge utile : `[type_lo, type_hi, …]`
/// -> `hw_type` u16 LE (ex. grip R = `0x0122`). Sert au contrôle de flash.
pub fn decode_hw_type(payload: &[u8]) -> Option<u16> {
    match (payload.first(), payload.get(1)) {
        (Some(&lo), Some(&hi)) => Some(lo as u16 | ((hi as u16) << 8)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pid_and_hardware_strings() {
        assert_eq!(catalog_pid(0x0a, 0xbf), 0xbf0a);
        assert_eq!(hardware_string(0x5100), "51.00");
        assert_eq!(hardware_string(0x0102), "1.02");
        assert_eq!(catalog_key(0xbf0a, "51.00"), "48906&51.00");
    }

    #[test]
    fn query_body_shape() {
        let body = build_query_body(&[(0xbf0a, "51.00".into())]);
        assert!(body.contains("\"pid\":48906"));
        assert!(body.contains("\"hardWare\":\"51.00\""));
        assert!(body.starts_with('[') && body.ends_with(']'));
    }

    #[test]
    fn parse_version_string_and_number() {
        let pid = 0xbf0a;
        let hw = "51.00";
        let key = catalog_key(pid, hw);
        let resp = format!(
            r#"{{"data":{{"{key}":{{"firmware_version":"1.0.7","file_name":"grip"}}}}}}"#
        );
        assert_eq!(parse_latest_version(&resp, pid, hw), Some("1.0.7".into()));
        let resp_n = format!(r#"{{"data":{{"{key}":{{"firmware_version":9}}}}}}"#);
        assert_eq!(parse_latest_version(&resp_n, pid, hw), Some("9".into()));
    }

    #[test]
    fn parse_absent_entry_is_none() {
        let resp = r#"{"data":{}}"#;
        assert_eq!(parse_latest_version(resp, 0xbf0a, "51.00"), None);
        assert_eq!(parse_latest_version("not json", 1, "x"), None);
    }

    #[test]
    fn decode_payloads() {
        // FW : [dev, fam, minor, major] -> "major.minor" (minor en hexa).
        assert_eq!(decode_fw_version(&[0x0a, 0xbf, 0x02, 0x01]).as_deref(), Some("1.02")); // grip R
        assert_eq!(decode_fw_version(&[0x10, 0xb9, 0x16, 0x01]).as_deref(), Some("1.16")); // throttle
        assert_eq!(decode_fw_version(&[0x0a, 0xbf]), None);
        // HW : [type_lo, type_hi, hw_lo, hw_hi] -> 0x5100
        assert_eq!(decode_hw(&[0x22, 0x01, 0x00, 0x51]), Some(0x5100));
        assert_eq!(decode_hw(&[0, 0]), None);
    }
}

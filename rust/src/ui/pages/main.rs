//! Page « Général » : bandeau d'identité du manche (photo + nom + série +
//! pastilles) et un repli « Détails techniques » (registres flash décodés).
//!
//! Read-only ; rend l'instantané partagé ([`PageState`]). Photos détourées
//! embarquées (rust/assets), choisies selon la main (droite/gauche).

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::flash;
use crate::i18n::{tr, tr_f};
use crate::model::{self, ControllerConfig, DeviceConfig, DeviceSnapshot, Transport};
use crate::protocol as p;
use crate::transport::HidrawTransport;

use super::{clear_box, escape, info_row, placeholder, scroll_area, Page, PageState};

const STICK_R: &[u8] = include_bytes!("../../../assets/stick_R.png");
const STICK_L: &[u8] = include_bytes!("../../../assets/stick_L.png");

pub struct MainPage {
    root: adw::ToastOverlay,
    content: gtk4::Box,
}

impl MainPage {
    pub fn new() -> Self {
        let (scroller, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&scroller));
        MainPage { root, content }
    }
}

impl Default for MainPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for MainPage {
    fn stack_id(&self) -> &'static str {
        "main"
    }
    fn title(&self) -> &'static str {
        "General"
    }
    fn icon_name(&self) -> &'static str {
        "dialog-information-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.root.clone().upcast()
    }
    fn set_state(&self, state: PageState) {
        clear_box(&self.content);
        match state {
            PageState::NoDevice => {
                placeholder(&self.content, &tr("Select a joystick from the list."));
            }
            PageState::Loading(dev) => {
                placeholder(&self.content, &tr_f("Reading {} …", &[dev.hidraw.as_str()]));
            }
            PageState::Error(_dev, msg) => {
                placeholder(
                    &self.content,
                    &tr_f("Read failed: {}\n(is the access rule installed?)", &[msg.as_str()]),
                );
            }
            PageState::Ready(dev, snap) => {
                self.content.append(&build_page(&dev, &snap, &self.root));
            }
        }
    }
}

fn build_page(dev: &WinwingDevice, snap: &DeviceSnapshot, overlay: &adw::ToastOverlay) -> gtk4::Widget {
    let cfg = &snap.config;
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    vbox.set_margin_top(20);
    vbox.set_margin_bottom(20);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    // Contrôleur d'identité : la poignée si présente, sinon le premier.
    let ident = cfg
        .controllers
        .iter()
        .find(|c| c.family == p::FAMILY_GRIP)
        .or_else(|| cfg.controllers.first());
    if let Some(g) = ident {
        vbox.append(&banner_card(g, cfg));
    }
    // Contrôle d'ÉCRITURE : mode de l'axe twist (poignée), derrière confirmation.
    if let Some(grip) = cfg.controllers.iter().find(|c| c.family == p::FAMILY_GRIP) {
        if let Some(mode) = grip.twist_mode {
            vbox.append(&twist_control(dev, grip.device, grip.family, mode, overlay));
        }
    }
    vbox.append(&deadzone_group(dev, snap, overlay));
    vbox.append(&fourx32_group(dev, snap, overlay));
    vbox.append(&firmware_group(dev));
    vbox.append(&details_group(cfg));
    vbox.append(&danger_group(dev, overlay));

    adw::Clamp::builder()
        .maximum_size(760)
        .child(&vbox)
        .build()
        .upcast()
}

// --- Firmware : vérification en ligne (lecture seule, aucun flash) ---------
const CATALOG_URL: &str = "https://winctrl.com/home/download/selectAll";
const CATALOG_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) ww-fw-fetch";

/// Groupe « Firmware » : un bouton qui lit HW/FW du manche et interroge le
/// catalogue WinWing pour la dernière version. Le travail (device + réseau) tourne
/// hors thread UI ; le résultat s'affiche dans la description du groupe.
fn firmware_group(dev: &WinwingDevice) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Firmware"));
    group.set_description(Some(&tr(
        "Checks the latest version in the WinWing catalog (read-only — no flashing).",
    )));
    let row = adw::ActionRow::builder()
        .title(tr("Check for the latest version"))
        .subtitle(tr("Reads the joystick's hardware/firmware then queries winctrl.com (network)"))
        .build();
    let btn = gtk4::Button::with_label(&tr("Check"));
    btn.set_valign(gtk4::Align::Center);
    row.add_suffix(&btn);
    row.set_activatable_widget(Some(&btn));
    {
        let dev = dev.clone();
        let group = group.clone();
        btn.connect_clicked(move |btn| {
            btn.set_sensitive(false);
            group.set_description(Some(&tr("Reading the joystick and querying the network…")));
            let (tx, rx) = async_channel::bounded::<String>(1);
            let dev = dev.clone();
            gio::spawn_blocking(move || {
                let _ = tx.send_blocking(check_firmware(&dev));
            });
            let group = group.clone();
            let btn = btn.clone();
            glib::spawn_future_local(async move {
                if let Ok(report) = rx.recv().await {
                    group.set_description(Some(&report));
                    btn.set_sensitive(true);
                }
            });
        });
    }
    group.add(&row);

    // Flashage — derrière confirmation destructive ; dry-run de validation d'abord.
    let flash_row = adw::ActionRow::builder()
        .title(tr("Flash a firmware…"))
        .subtitle(tr("Irreversible operation — use only an official WinWing firmware."))
        .build();
    let flash_btn = gtk4::Button::with_label(&tr("Flash…"));
    flash_btn.add_css_class("destructive-action");
    flash_btn.set_valign(gtk4::Align::Center);
    flash_row.add_suffix(&flash_btn);
    {
        let dev = dev.clone();
        flash_btn.connect_clicked(move |btn| {
            let parent = btn.root().and_downcast::<gtk4::Window>();
            let parent2 = parent.clone();
            let dev = dev.clone();
            let dialog = gtk4::FileDialog::builder()
                .title(tr("Choose a .wwtc firmware"))
                .build();
            dialog.open(parent.as_ref(), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                flash_validate_then_confirm(dev.clone(), parent2.clone(), path);
            });
        });
    }
    group.add(&flash_row);
    group
}

// --- Flashage : validation (dry-run) → confirmation → exécution ------------
/// Valide le `.wwtc` contre le contrôleur (hors thread UI), puis propose la
/// confirmation destructive ou signale le refus.
fn flash_validate_then_confirm(
    dev: WinwingDevice,
    parent: Option<gtk4::Window>,
    path: std::path::PathBuf,
) {
    let (tx, rx) =
        async_channel::bounded::<Result<(flash::FlashTarget, flash::Firmware, String), String>>(1);
    let dev2 = dev.clone();
    gio::spawn_blocking(move || {
        let _ = tx.send_blocking(flash_validate(&dev2, &path));
    });
    glib::spawn_future_local(async move {
        if let Ok(res) = rx.recv().await {
            match res {
                Ok((target, fw, report)) => show_flash_confirm(dev, parent, target, fw, report),
                Err(e) => show_flash_message(parent.as_ref(), &tr("Firmware refused"), &e),
            }
        }
    });
}

/// Lecture + parsing + validation contre le matériel + dry-run. **Bloquant.**
fn flash_validate(
    dev: &WinwingDevice,
    path: &std::path::Path,
) -> Result<(flash::FlashTarget, flash::Firmware, String), String> {
    let bytes = std::fs::read(path).map_err(|e| tr_f("Reading the file: {}", &[e.to_string().as_str()]))?;
    let fw = flash::parse_wwtc(&bytes)?;
    let ctl = dev
        .controllers
        .iter()
        .find(|c| c.device == fw.header.device && c.family == fw.header.family)
        .ok_or_else(|| {
            tr_f(
                "No {}/{} controller on this joystick",
                &[
                    format!("{:#04x}", fw.header.device).as_str(),
                    format!("{:#04x}", fw.header.family).as_str(),
                ],
            )
        })?;
    let mut t = HidrawTransport::open(&dev.hidraw).map_err(|e| tr_f("opening: {}", &[e.to_string().as_str()]))?;
    let hw = t
        .request(ctl.device, ctl.family, p::OP_REQUEST_DEVICE_HW)
        .ok_or_else(|| tr("no hardware response from the controller"))?;
    let hwver = crate::firmware::decode_hw(&hw).ok_or_else(|| tr("unreadable hardware version"))?;
    let hw_type = crate::firmware::decode_hw_type(&hw).ok_or_else(|| tr("unreadable hardware type"))?;
    let target = flash::FlashTarget {
        device: ctl.device,
        family: ctl.family,
        hwver,
        hw_type,
    };
    flash::check_target(&fw, &target)?;
    let mut lines = Vec::new();
    flash::run_flash(dev, &target, &fw, &flash::FlashOptions { dry_run: true }, |pg| {
        if let flash::FlashProgress::Info(s) = pg {
            lines.push(s);
        }
    })?;
    Ok((target, fw, lines.join("\n")))
}

fn show_flash_message(parent: Option<&gtk4::Window>, heading: &str, body: &str) {
    let d = adw::AlertDialog::new(Some(heading), Some(body));
    d.add_response("ok", &tr("Close"));
    d.present(parent);
}

/// Dialogue de confirmation **destructif** avant tout flash réel.
fn show_flash_confirm(
    dev: WinwingDevice,
    parent: Option<gtk4::Window>,
    target: flash::FlashTarget,
    fw: flash::Firmware,
    report: String,
) {
    let body = format!(
        "{report}\n\n{}",
        tr(
            "Warning — irreversible operation: it rewrites the controller's memory. \
             Use only an official WinWing firmware and do not unplug anything during \
             the operation (~1 min). An interruption can make the joystick unusable."
        )
    );
    let d = adw::AlertDialog::new(Some(&tr("Flash the firmware?")), Some(&body));
    d.add_response("cancel", &tr("Cancel"));
    d.add_response("flash", &tr("Flash for real"));
    d.set_response_appearance("flash", adw::ResponseAppearance::Destructive);
    d.set_default_response(Some("cancel"));
    d.set_close_response("cancel");
    let parent2 = parent.clone();
    d.connect_response(None, move |_, resp| {
        if resp == "flash" {
            flash_execute(dev.clone(), parent2.clone(), target, fw.clone());
        }
    });
    d.present(parent.as_ref());
}

/// Exécute le flash réel (worker) et affiche la progression puis le résultat.
fn flash_execute(
    dev: WinwingDevice,
    parent: Option<gtk4::Window>,
    target: flash::FlashTarget,
    fw: flash::Firmware,
) {
    let prog = adw::AlertDialog::new(
        Some(&tr("Flashing…")),
        Some(&tr("Preparing… Do not unplug the joystick.")),
    );
    prog.add_response("close", &tr("Close"));
    prog.set_response_enabled("close", false);
    prog.present(parent.as_ref());

    let (ptx, prx) = async_channel::bounded::<flash::FlashProgress>(64);
    let (dtx, drx) = async_channel::bounded::<Result<(), String>>(1);
    let dev2 = dev.clone();
    gio::spawn_blocking(move || {
        let res = flash::run_flash(&dev2, &target, &fw, &flash::FlashOptions { dry_run: false }, |pg| {
            let _ = ptx.send_blocking(pg);
        });
        let _ = dtx.send_blocking(res);
    });

    let prog_p = prog.clone();
    glib::spawn_future_local(async move {
        while let Ok(pg) = prx.recv().await {
            match pg {
                flash::FlashProgress::Info(s) => prog_p.set_body(&s),
                flash::FlashProgress::Writing(done, total) => prog_p.set_body(&tr_f(
                    "Writing: {} / {} bytes",
                    &[done.to_string().as_str(), total.to_string().as_str()],
                )),
            }
        }
    });
    let prog_d = prog.clone();
    glib::spawn_future_local(async move {
        if let Ok(res) = drx.recv().await {
            match res {
                Ok(()) => {
                    prog_d.set_heading(Some(&tr("Flash complete")));
                    prog_d.set_body(&tr("The controller is back in application mode."));
                }
                Err(e) => {
                    prog_d.set_heading(Some(&tr("Flash failed")));
                    prog_d.set_body(&tr_f(
                        "{}\n\nIf the joystick no longer responds, run a flash again with an \
                         official firmware to recover it.",
                        &[e.as_str()],
                    ));
                }
            }
            prog_d.set_response_enabled("close", true);
        }
    });
}

/// Lit HW/FW de chaque contrôleur et compare au catalogue. **Bloquant** (device +
/// réseau `curl`) : à lancer hors thread UI. Rend un rapport multi-lignes.
fn check_firmware(dev: &WinwingDevice) -> String {
    use crate::firmware as fw;
    let mut t = match HidrawTransport::open(&dev.hidraw) {
        Ok(t) => t,
        Err(e) => return tr_f("Cannot open the joystick: {}", &[e.to_string().as_str()]),
    };
    struct Ctl {
        device: u8,
        family: u8,
        fw: Option<String>,
        hwver: Option<u16>,
    }
    let ctls: Vec<Ctl> = dev
        .controllers
        .iter()
        .map(|c| Ctl {
            device: c.device,
            family: c.family,
            fw: t
                .request(c.device, c.family, p::OP_REQUEST_DEVICE_FW)
                .and_then(|pl| fw::decode_fw_version(&pl)),
            hwver: t
                .request(c.device, c.family, p::OP_REQUEST_DEVICE_HW)
                .and_then(|pl| fw::decode_hw(&pl)),
        })
        .collect();

    let items: Vec<(u32, String)> = ctls
        .iter()
        .filter_map(|c| c.hwver.map(|hw| (fw::catalog_pid(c.device, c.family), fw::hardware_string(hw))))
        .collect();
    if items.is_empty() {
        return tr("Unreadable hardware version on this joystick.");
    }
    let body = fw::build_query_body(&items);
    let resp = match curl_post(CATALOG_URL, &body) {
        Ok(r) => r,
        Err(e) => return tr_f("Catalog request failed: {}", &[e.to_string().as_str()]),
    };

    let mut lines: Vec<String> = Vec::new();
    for c in &ctls {
        let Some(hw) = c.hwver else { continue };
        let pid = fw::catalog_pid(c.device, c.family);
        let hwstr = fw::hardware_string(hw);
        let name = if c.family == p::FAMILY_GRIP { tr("Grip") } else { tr("Base") };
        let cur = c.fw.clone().unwrap_or_else(|| "?".into());
        match fw::parse_latest_version(&resp, pid, &hwstr) {
            Some(latest) => {
                // Device et catalogue sont au même format « major.minor » : verdict fiable.
                let verdict = match &c.fw {
                    Some(v) if *v == latest => tr(" — up to date"),
                    Some(_) => tr(" — update available (flashing not implemented)"),
                    None => String::new(),
                };
                lines.push(tr_f(
                    "{} (HW {}): firmware {} · catalog {}{}",
                    &[
                        name.as_str(),
                        hwstr.as_str(),
                        cur.as_str(),
                        latest.as_str(),
                        verdict.as_str(),
                    ],
                ));
            }
            None => lines.push(tr_f(
                "{} (HW {}): firmware {} · no catalog entry",
                &[name.as_str(), hwstr.as_str(), cur.as_str()],
            )),
        }
    }
    lines.join("\n")
}

/// POST JSON via `curl` (léger, pas de pile TLS Rust). UA navigateur (Cloudflare).
fn curl_post(url: &str, body: &str) -> std::io::Result<String> {
    let out = std::process::Command::new("curl")
        .args([
            "-s",
            "--max-time",
            "20",
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "-H",
            &format!("User-Agent: {CATALOG_UA}"),
            "-d",
            body,
            url,
        ])
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(tr_f(
            "curl failed (code {}) — network/curl unavailable?",
            &[format!("{:?}", out.status.code()).as_str()],
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn base_ctrl(dev: &WinwingDevice) -> (u8, u8) {
    dev.controllers
        .iter()
        .find(|c| c.family == p::FAMILY_BASE)
        .map(|c| (c.device, c.family))
        .unwrap_or((p::DEVICE_BASE, p::FAMILY_BASE))
}

fn grip_ctrl(dev: &WinwingDevice) -> Option<(u8, u8)> {
    dev.controllers
        .iter()
        .find(|c| c.family == p::FAMILY_GRIP)
        .map(|c| (c.device, c.family))
}

/// Ligne « valeur + Appliquer » pour une écriture gardée d'un entier.
#[allow(clippy::too_many_arguments)]
fn spin_apply_row(
    title: &str,
    subtitle: &str,
    value: f64,
    max: f64,
    overlay: &adw::ToastOverlay,
    path: String,
    device: u8,
    family: u8,
    make: impl Fn(u32) -> super::WriteAction + 'static,
    heading: String,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder().title(title).subtitle(subtitle).build();
    let spin = gtk4::SpinButton::with_range(0.0, max, 1.0);
    spin.set_value(value);
    spin.set_valign(gtk4::Align::Center);
    let btn = gtk4::Button::with_label(&tr("Apply"));
    btn.add_css_class("flat");
    btn.set_valign(gtk4::Align::Center);
    {
        let overlay = overlay.clone();
        let spin = spin.clone();
        btn.connect_clicked(move |_| {
            let v = spin.value() as u32;
            super::confirm_write(
                &overlay,
                &heading,
                &tr_f("Write the value {}?\n\nAutomatic backup first. Reversible.", &[v.to_string().as_str()]),
                false,
                path.clone(),
                device,
                family,
                make(v),
                std::rc::Rc::new(|| {}),
            );
        });
    }
    row.add_suffix(&spin);
    row.add_suffix(&btn);
    row
}

/// Sous-titre d'une zone morte : sa valeur si elle est réellement posée, sinon
/// « désactivée » (au lieu de la valeur sentinelle brute).
fn deadzone_subtitle(value: Option<u32>) -> String {
    match value {
        Some(v) => tr_f("Deadzone active: {}", &[v.to_string().as_str()]),
        None => tr("Disabled (no deadzone)"),
    }
}

/// Zones mortes des axes de la base (X, Y) et du lacet de la poignée.
fn deadzone_group(
    dev: &WinwingDevice,
    snap: &DeviceSnapshot,
    overlay: &adw::ToastOverlay,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Deadzones"));
    group.set_description(Some(&tr("Automatic backup before every write.")));
    let (bd, bf) = base_ctrl(dev);
    let path = dev.hidraw.clone();
    let dx = model::base_deadzone_display(snap.deadzone_x);
    group.add(&spin_apply_row(
        &tr("X axis (base)"),
        &deadzone_subtitle(dx),
        dx.unwrap_or(0) as f64,
        65535.0,
        overlay,
        path.clone(),
        bd,
        bf,
        |v| super::WriteAction::DeadzoneBase { y_axis: false, value: v },
        tr("X axis deadzone"),
    ));
    let dy = model::base_deadzone_display(snap.deadzone_y);
    group.add(&spin_apply_row(
        &tr("Y axis (base)"),
        &deadzone_subtitle(dy),
        dy.unwrap_or(0) as f64,
        65535.0,
        overlay,
        path.clone(),
        bd,
        bf,
        |v| super::WriteAction::DeadzoneBase { y_axis: true, value: v },
        tr("Y axis deadzone"),
    ));
    if let Some((gd, gf)) = grip_ctrl(dev) {
        let dz = model::twist_deadzone_display(snap.twist_deadzone);
        group.add(&spin_apply_row(
            &tr("Yaw (rotation)"),
            &deadzone_subtitle(dz.map(u32::from)),
            dz.unwrap_or(0) as f64,
            30.0,
            overlay,
            path,
            gd,
            gf,
            |v| super::WriteAction::DeadzoneTwist(v as u8),
            tr("Yaw deadzone"),
        ));
    }
    group
}

/// D4 : mode 4x32 de la base (redémarrage requis).
fn fourx32_group(
    dev: &WinwingDevice,
    snap: &DeviceSnapshot,
    overlay: &adw::ToastOverlay,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("4×32 mode"));
    group.set_description(Some(&tr(
        "Splits the buttons into blocks of 32, useful for games that only handle 32 buttons.",
    )));
    let (bd, bf) = base_ctrl(dev);
    let on = snap.base_4x32.unwrap_or(false);
    let row = adw::ActionRow::builder()
        .title(if on { tr("Enabled") } else { tr("Disabled") })
        .subtitle(tr("The joystick is restarted after the write"))
        .build();
    let btn = gtk4::Button::with_label(&(if on { tr("Disable") } else { tr("Enable") }));
    btn.add_css_class("flat");
    btn.set_valign(gtk4::Align::Center);
    {
        let overlay = overlay.clone();
        let path = dev.hidraw.clone();
        btn.connect_clicked(move |_| {
            super::confirm_write(
                &overlay,
                &tr("Change 4×32 mode?"),
                &tr("The joystick RESTARTS after the write (USB re-enumeration).\n\nAutomatic backup first."),
                false,
                path.clone(),
                bd,
                bf,
                super::WriteAction::FourX32(!on),
                std::rc::Rc::new(|| {}),
            );
        });
    }
    row.add_suffix(&btn);
    group.add(&row);
    group
}

/// D6 : réinitialisation usine (0xB4) — DOUBLE confirmation, destructif.
fn danger_group(dev: &WinwingDevice, overlay: &adw::ToastOverlay) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Danger zone"));
    let Some((gd, gf)) = grip_ctrl(dev) else {
        return group;
    };
    let row = adw::ActionRow::builder()
        .title(tr("Factory reset"))
        .subtitle(tr("Resets the entire grip configuration to factory values. Broad and hardly reversible."))
        .build();
    let btn = gtk4::Button::with_label(&tr("Reset…"));
    btn.add_css_class("destructive-action");
    btn.set_valign(gtk4::Align::Center);
    {
        let overlay = overlay.clone();
        let path = dev.hidraw.clone();
        btn.connect_clicked(move |_| {
            // 1re confirmation (avertissement fort).
            let d1 = adw::AlertDialog::new(
                Some(&tr("Factory reset")),
                Some(&tr("This resets the ENTIRE grip configuration (calibration included) to factory values, then restarts the joystick. Hardly reversible.\n\nContinue?")),
            );
            d1.add_response("cancel", &tr("Cancel"));
            d1.add_response("go", &tr("Continue"));
            d1.set_response_appearance("go", adw::ResponseAppearance::Destructive);
            d1.set_default_response(Some("cancel"));
            d1.set_close_response("cancel");
            let ov = overlay.clone();
            let path2 = path.clone();
            d1.connect_response(None, move |_dlg, resp| {
                if resp != "go" {
                    return;
                }
                super::confirm_write(
                    &ov,
                    &tr("Confirm the reset?"),
                    &tr("A full timestamped backup is created first. The joystick then restarts."),
                    true,
                    path2.clone(),
                    gd,
                    gf,
                    super::WriteAction::RestoreDefault,
                    std::rc::Rc::new(|| {}),
                );
            });
            d1.present(Some(&overlay));
        });
    }
    row.add_suffix(&btn);
    group.add(&row);
    group
}

fn field_human<'a>(c: &'a ControllerConfig, name: &str) -> Option<&'a str> {
    c.fields.iter().find(|f| f.name == name).map(|f| f.human.as_str())
}

/// Bandeau : photo du manche + identité + pastilles.
fn banner_card(ident: &ControllerConfig, cfg: &DeviceConfig) -> gtk4::Widget {
    let card = gtk4::Box::new(gtk4::Orientation::Horizontal, 18);
    card.add_css_class("wl-card");
    set_all_margins(&card, 15);

    // Tuile photo (droite/gauche selon le modèle).
    let is_left = cfg.controllers.iter().any(|c| c.model.ends_with("_L"));
    let bytes = if is_left { STICK_L } else { STICK_R };
    let tile = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    tile.add_css_class("wl-stick-tile");
    tile.set_size_request(124, 124);
    let pic = stick_picture(bytes);
    pic.set_halign(gtk4::Align::Center);
    pic.set_valign(gtk4::Align::Center);
    pic.set_hexpand(true);
    tile.append(&pic);
    card.append(&tile);

    // Colonne d'identité.
    let col = gtk4::Box::new(gtk4::Orientation::Vertical, 5);
    col.set_valign(gtk4::Align::Center);
    col.set_hexpand(true);

    // Nom commercial neutre (le nom firmware/technique reste sous « Détails »).
    let name = tr(&p::commercial_name(ident.model));
    let name_lbl = gtk4::Label::new(None);
    name_lbl.set_markup(&format!(
        "<span size='x-large' weight='bold'>{}</span>",
        escape(&name)
    ));
    name_lbl.set_halign(gtk4::Align::Start);
    col.append(&name_lbl);

    // Sous-titre humain : nature des contrôleurs présents + numéro de série court.
    let parts = cfg
        .controllers
        .iter()
        .map(|c| if c.family == p::FAMILY_GRIP { tr("Grip") } else { tr("Base") })
        .collect::<Vec<_>>()
        .join(" + ");
    let serial_short: String = ident.serial.chars().take(8).collect();
    let sub = gtk4::Label::new(Some(
        &tr_f("{} · serial no. {}…", &[parts.as_str(), serial_short.as_str()]),
    ));
    sub.add_css_class("dim-label");
    sub.set_halign(gtk4::Align::Start);
    col.append(&sub);

    // Pastilles dérivées du snapshot.
    let pills = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    pills.set_margin_top(3);
    if let Some(hw) = field_human(ident, "hardware_version") {
        pills.append(&pill(hw, "neutral"));
    }
    if field_human(ident, "firmware_complete") == Some("Firmware complete") {
        pills.append(&pill(&tr("Full firmware"), "neutral"));
    }
    if ident.twist_mode.is_some() {
        let yaw = tr_f("Yaw: {}", &[tr(&ident.twist_label()).as_str()]);
        pills.append(&pill(&yaw, "accent"));
    }
    col.append(&pills);
    card.append(&col);

    card.upcast()
}

/// Charge le PNG embarqué, mis à l'échelle à ~108 px (taille naturelle bornée).
fn stick_picture(bytes: &'static [u8]) -> gtk4::Picture {
    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gtk4::glib::Bytes::from_static(bytes));
    let pic = gtk4::Picture::new();
    if let Ok(pb) = gtk4::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        -1,
        108,
        true,
        gtk4::gio::Cancellable::NONE,
    ) {
        pic.set_paintable(Some(&gtk4::gdk::Texture::for_pixbuf(&pb)));
    }
    pic
}

fn pill(text: &str, kind: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.add_css_class("wl-pill");
    l.add_css_class(kind);
    l.set_valign(gtk4::Align::Center);
    l
}

fn set_all_margins(w: &impl IsA<gtk4::Widget>, m: i32) {
    w.set_margin_top(m);
    w.set_margin_bottom(m);
    w.set_margin_start(m);
    w.set_margin_end(m);
}

/// Contrôle d'écriture du mode twist Z (0xD8) : segment 3 modes ; chaque clic
/// passe par une confirmation avant d'appliquer (backup → diff → écho → relecture).
fn twist_control(
    dev: &WinwingDevice,
    device: u8,
    family: u8,
    current_mode: u8,
    overlay: &adw::ToastOverlay,
) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Yaw axis"));
    group.set_description(Some(&tr(
        "Mode of the rotation axis. Reversible write, automatic backup first.",
    )));
    let row = adw::ActionRow::builder().title(tr("Mode")).build();
    let seg = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    seg.add_css_class("linked");
    seg.set_homogeneous(true);
    seg.set_valign(gtk4::Align::Center);
    for (label, mode) in [
        (tr("Buttons"), 0x00u8),
        (tr("Axis + buttons"), 0x01),
        (tr("Axis only"), 0xFF),
    ] {
        let btn = gtk4::Button::with_label(&label);
        if mode == current_mode {
            btn.add_css_class("wl-seg-active");
        }
        let path = dev.hidraw.clone();
        let overlay = overlay.clone();
        let seg2 = seg.clone();
        let btn2 = btn.clone();
        btn.connect_clicked(move |_| {
            confirm_and_apply(&overlay, &seg2, &btn2, path.clone(), device, family, mode, label.clone());
        });
        seg.append(&btn);
    }
    row.add_suffix(&seg);
    group.add(&row);
    group
}

/// Dialogue de confirmation puis application (sur worker) d'un mode twist.
#[allow(clippy::too_many_arguments)]
fn confirm_and_apply(
    overlay: &adw::ToastOverlay,
    seg: &gtk4::Box,
    btn: &gtk4::Button,
    path: String,
    device: u8,
    family: u8,
    mode: u8,
    label: String,
) {
    let dialog = adw::AlertDialog::new(
        Some(&tr("Change the yaw mode?")),
        Some(&tr_f(
            "Write \"{}\" for the yaw axis?\n\nA backup is created first. Reversible operation.",
            &[label.as_str()],
        )),
    );
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("apply", &tr("Write"));
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let overlay_c = overlay.clone();
    let seg = seg.clone();
    let btn = btn.clone();
    dialog.connect_response(None, move |_dlg, resp| {
        if resp != "apply" {
            return;
        }
        let label = label.clone();
        let ts = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y%m%d-%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let path = path.clone();
        let (tx, rx) = async_channel::bounded::<Result<model::WriteOutcome, String>>(1);
        // Worker : ouverture + écriture gardée HORS thread UI.
        gio::spawn_blocking(move || {
            let res = HidrawTransport::open(&path).map_err(|e| e.to_string()).and_then(|mut t| {
                model::set_twist_mode(&mut t, device, family, mode, false, Some(&ts))
                    .map_err(|e| e.to_string())
            });
            let _ = tx.send_blocking(res);
        });
        let overlay = overlay_c.clone();
        let seg = seg.clone();
        let btn = btn.clone();
        glib::spawn_future_local(async move {
            if let Ok(res) = rx.recv().await {
                match res {
                    Ok(out) => {
                        let msg = if out.skipped {
                            tr_f("Already \"{}\" — nothing to write", &[label.as_str()])
                        } else if out.verified {
                            tr_f("Yaw mode written: {}", &[label.as_str()])
                        } else if out.emitted {
                            tr_f("Written but not confirmed: {}", &[label.as_str()])
                        } else {
                            tr_f("Nothing written: {}", &[label.as_str()])
                        };
                        overlay.add_toast(adw::Toast::new(&msg));
                        if out.verified || out.skipped {
                            let mut child = seg.first_child();
                            while let Some(c) = child {
                                c.remove_css_class("wl-seg-active");
                                child = c.next_sibling();
                            }
                            btn.add_css_class("wl-seg-active");
                        }
                    }
                    Err(e) => overlay.add_toast(adw::Toast::new(&tr_f("Failed: {}", &[e.as_str()]))),
                }
            }
        });
    });
    dialog.present(Some(overlay));
}

/// Groupe repliable « Détails techniques » : un expander par contrôleur avec
/// ses registres flash décodés.
fn details_group(cfg: &DeviceConfig) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Technical details"));
    group.set_description(Some(&tr("Decoded flash registers (read-only).")));
    for c in &cfg.controllers {
        let exp = adw::ExpanderRow::builder()
            .title(escape(&format!(
                "{} (dev {:#04x} / fam {:#04x})",
                c.model, c.device, c.family
            )))
            .build();
        if !c.product_name.is_empty() {
            exp.add_row(&info_row(&tr("Product"), &c.product_name));
        }
        if !c.serial.is_empty() {
            exp.add_row(&info_row(&tr("Serial number"), &c.serial));
        }
        for f in &c.fields {
            let row = adw::ActionRow::builder()
                .title(escape(f.name))
                .subtitle(escape(&format!("{}   {}", f.hex(), f.human)))
                .build();
            row.add_css_class("property");
            let off = gtk4::Label::new(Some(&format!("{:#06x}", f.offset)));
            off.add_css_class("dim-label");
            off.add_css_class("numeric");
            row.add_prefix(&off);
            if f.identity {
                // Registre d'identité protégé (jamais écrit).
                let lock = gtk4::Image::from_icon_name("channel-secure-symbolic");
                lock.add_css_class("dim-label");
                lock.set_tooltip_text(Some(&tr("Protected identity register (never written)")));
                row.add_suffix(&lock);
            }
            exp.add_row(&row);
        }
        group.add(&exp);
    }
    group
}

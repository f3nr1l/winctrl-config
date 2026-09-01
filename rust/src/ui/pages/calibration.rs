//! Page « Calibration » : assistant 0x47/0x48 (D5) + région courante.
//!
//! Par axe calibrable : un bouton « Calibrer » lance un assistant en 2 temps —
//! (1) confirmation + backup + `CALIBRATION_START` (0x47) ; (2) après que
//! l'utilisateur a balayé l'axe, `CALIBRATION_FINISH` (0x48) ; le FIRMWARE écrit
//! min/centre/max en 0xC8–0xF8 (l'hôte n'écrit rien lui-même). I/O sur worker.
//! La région brute reste consultable (repli). Aucune écriture émise par moi.

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::model::{self, ControllerCalib, Transport};
use crate::protocol as p;
use crate::transport::HidrawTransport;

use super::{clear_box, info_row, placeholder, scroll_area, Page, PageState};

type Region = Vec<(u32, Option<[u8; 4]>)>;

pub struct CalibrationPage {
    root: adw::ToastOverlay,
    content: gtk4::Box,
}

impl CalibrationPage {
    pub fn new() -> Self {
        let (scroller, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&scroller));
        CalibrationPage { root, content }
    }
}

impl Default for CalibrationPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for CalibrationPage {
    fn stack_id(&self) -> &'static str {
        "calibration"
    }
    fn title(&self) -> &'static str {
        "Calibration"
    }
    fn icon_name(&self) -> &'static str {
        "preferences-system-symbolic"
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
                placeholder(&self.content, &tr_f("Read failed: {}", &[msg.as_str()]));
            }
            PageState::Ready(dev, snap) => {
                self.content.append(&build_page(&dev, &snap.calib, &self.root));
            }
        }
    }
}

fn build_page(
    dev: &WinwingDevice,
    calib: &[ControllerCalib],
    overlay: &adw::ToastOverlay,
) -> gtk4::Widget {
    let page = adw::PreferencesPage::new();
    let path = dev.hidraw.clone();

    for c in calib {
        let group = adw::PreferencesGroup::new();
        group.set_title(&(if c.family == p::FAMILY_GRIP { tr("Grip") } else { tr("Base") }));

        if c.axes.is_empty() {
            group.add(&info_row(&tr("Axes"), &tr("no calibratable axis")));
        }
        for &axis in &c.axes {
            let Some(index) = p::axis_name_index(axis) else {
                continue;
            };
            let row = adw::ActionRow::builder()
                .title(tr_f("{} axis", &[axis]))
                .build();
            let btn = gtk4::Button::with_label(&tr("Calibrate…"));
            btn.add_css_class("flat");
            btn.set_valign(gtk4::Align::Center);
            let overlay = overlay.clone();
            let path = path.clone();
            let (device, family) = (c.device, c.family);
            btn.connect_clicked(move |_| {
                calibrate_flow(&overlay, path.clone(), device, family, index, axis);
            });
            row.add_suffix(&btn);
            group.add(&row);
        }

        // Valeurs brutes (repli, pour diagnostic).
        if !c.region.is_empty() {
            let exp = adw::ExpanderRow::builder()
                .title(tr("Technical details (raw values)"))
                .build();
            for (off, raw) in &c.region {
                let hex = match raw {
                    Some(b) => p::hx(b),
                    None => "-- -- -- --".to_string(),
                };
                let r = adw::ActionRow::builder()
                    .title(format!("{off:#06x}"))
                    .subtitle(hex)
                    .build();
                r.add_css_class("property");
                exp.add_row(&r);
            }
            group.add(&exp);
        }
        page.add(&group);
    }

    let note = adw::PreferencesGroup::new();
    let row = adw::ActionRow::builder()
        .title(tr("Calibration wizard"))
        .subtitle(tr("\"Calibrate\" creates a backup then starts calibration; after sweeping the axis, \"Finish\" records the end stops and center in the joystick."))
        .build();
    row.add_css_class("property");
    note.add(&row);
    page.add(&note);

    page.upcast()
}

/// Étape 1 : confirmation → backup + start (0x47), puis dialogue de balayage.
fn calibrate_flow(
    overlay: &adw::ToastOverlay,
    path: String,
    device: u8,
    family: u8,
    index: u8,
    axis: &'static str,
) {
    let d1 = adw::AlertDialog::new(
        Some(&tr_f("Calibrate the {} axis?", &[axis])),
        Some(&tr("A timestamped backup is created, then calibration starts. You will then sweep the axis fully and re-center it.")),
    );
    d1.add_response("cancel", &tr("Cancel"));
    d1.add_response("start", &tr("Start"));
    d1.set_response_appearance("start", adw::ResponseAppearance::Suggested);
    d1.set_default_response(Some("cancel"));
    d1.set_close_response("cancel");

    let ov = overlay.clone();
    d1.connect_response(None, move |_dlg, resp| {
        if resp != "start" {
            return;
        }
        let ts = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y%m%d-%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let path2 = path.clone();
        let (tx, rx) = async_channel::bounded::<Result<Region, String>>(1);
        gio::spawn_blocking(move || {
            let res = HidrawTransport::open(&path2).map_err(|e| e.to_string()).map(|mut t| {
                let _ = model::backup_controller(&mut t, device, family, &ts);
                let _ = t.start_calibration(device, family, index);
                model::read_calib_region(&mut t, device, family) // état AVANT
            });
            let _ = tx.send_blocking(res);
        });
        let ov2 = ov.clone();
        let path = path.clone();
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(before)) => sweep_dialog(&ov2, path, device, family, index, axis, before),
                Ok(Err(e)) => ov2.add_toast(adw::Toast::new(&tr_f("Startup failed: {}", &[e.as_str()]))),
                Err(_) => {}
            }
        });
    });
    d1.present(Some(overlay));
}

/// Étape 2 : balayage → finish (0x48) + diff de la région → toast.
fn sweep_dialog(
    overlay: &adw::ToastOverlay,
    path: String,
    device: u8,
    family: u8,
    index: u8,
    axis: &'static str,
    before: Region,
) {
    let d2 = adw::AlertDialog::new(
        Some(&tr_f("Sweep the {} axis", &[axis])),
        Some(&tr("Push the axis fully in every direction, then release it to the center. Click \"Finish\": the firmware records min/center/max.")),
    );
    d2.add_response("cancel", &tr("Cancel"));
    d2.add_response("finish", &tr("Finish"));
    d2.set_response_appearance("finish", adw::ResponseAppearance::Suggested);
    d2.set_default_response(Some("finish"));
    d2.set_close_response("cancel");

    let ov = overlay.clone();
    d2.connect_response(None, move |_dlg, resp| {
        if resp != "finish" {
            return;
        }
        let path2 = path.clone();
        let before = before.clone();
        let (tx, rx) = async_channel::bounded::<Result<usize, String>>(1);
        gio::spawn_blocking(move || {
            let res = HidrawTransport::open(&path2).map_err(|e| e.to_string()).map(|mut t| {
                let _ = t.finish_calibration(device, family, index);
                let after = model::read_calib_region(&mut t, device, family);
                before
                    .iter()
                    .zip(after.iter())
                    .filter(|((_, a), (_, b))| a != b)
                    .count()
            });
            let _ = tx.send_blocking(res);
        });
        let ov2 = ov.clone();
        glib::spawn_future_local(async move {
            match rx.recv().await {
                Ok(Ok(n)) => {
                    let msg = if n > 0 {
                        tr_f("Calibration of the {} axis complete", &[axis])
                    } else {
                        tr_f("{} axis: no change recorded", &[axis])
                    };
                    ov2.add_toast(adw::Toast::new(&msg));
                }
                Ok(Err(e)) => ov2.add_toast(adw::Toast::new(&tr_f("Failed: {}", &[e.as_str()]))),
                Err(_) => {}
            }
        });
    });
    d2.present(Some(overlay));
}

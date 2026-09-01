//! Page « Rétroéclairage » : contrôle de la lumière d'ambiance de la base.
//!
//! Mise en page canvas (tuile photo + contrôles). ÉCRITURES (D2) :
//! - Luminosité LIVE (SET_LEDX 0x49, non-flash, réversible) : le slider envoie
//!   en continu pendant le drag, SANS confirmation (rien n'est persisté).
//! - PERSISTANCE 0xEC et MODE 0xF8 (respiration/fixe/éteint) : flash → derrière
//!   un Adw.AlertDialog de confirmation (backup → diff → écho → relecture),
//!   résultat en Adw.Toast. I/O sur worker.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::model::{self, BaseLedState, Transport};
use crate::protocol as p;
use crate::transport::HidrawTransport;

use super::{clear_box, info_row, placeholder, scroll_area, Page, PageState};

const BASE_BL: &[u8] = include_bytes!("../../../assets/base_backlight.png");

/// Écriture flash de rétroéclairage possible (valeur Copy pour le handler).
#[derive(Clone, Copy)]
enum BlWrite {
    /// Luminosité persistée 0xEC.
    Brightness(u8),
    /// Mode respiration (true) / fixe (false), 0xF8.
    Breathing(bool),
}

pub struct BacklightPage {
    root: adw::ToastOverlay,
    content: gtk4::Box,
}

impl BacklightPage {
    pub fn new() -> Self {
        let (scroller, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        let root = adw::ToastOverlay::new();
        root.set_child(Some(&scroller));
        BacklightPage { root, content }
    }
}

impl Default for BacklightPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for BacklightPage {
    fn stack_id(&self) -> &'static str {
        "backlight"
    }
    fn title(&self) -> &'static str {
        "Backlight"
    }
    fn icon_name(&self) -> &'static str {
        "display-brightness-symbolic"
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
                self.content.append(&build_page(&dev, snap.base_led, &self.root));
            }
        }
    }
}

fn build_page(
    dev: &WinwingDevice,
    led: Option<BaseLedState>,
    overlay: &adw::ToastOverlay,
) -> gtk4::Widget {
    let (device, family) = dev
        .controllers
        .iter()
        .find(|c| c.family == p::FAMILY_BASE)
        .map(|c| (c.device, c.family))
        .unwrap_or((p::DEVICE_BASE, p::FAMILY_BASE));
    let path = dev.hidraw.clone();

    let outer = gtk4::Box::new(gtk4::Orientation::Vertical, 14);
    outer.set_margin_top(20);
    outer.set_margin_bottom(20);
    outer.set_margin_start(16);
    outer.set_margin_end(16);

    let cols = gtk4::Box::new(gtk4::Orientation::Horizontal, 20);
    cols.set_valign(gtk4::Align::Center);

    // --- Gauche : tuile photo sombre --------------------------------------
    let tile = gtk4::Box::new(gtk4::Orientation::Vertical, 12);
    tile.add_css_class("wl-phototile");
    tile.set_hexpand(true);
    tile.set_valign(gtk4::Align::Center);
    let pic = base_picture();
    pic.set_halign(gtk4::Align::Center);
    tile.append(&pic);
    let cap = gtk4::Label::new(Some(tr("Preview · base backlight").as_str()));
    cap.add_css_class("wl-phototile-caption");
    cap.set_halign(gtk4::Align::Center);
    tile.append(&cap);
    cols.append(&tile);

    // --- Droite : contrôles -----------------------------------------------
    let right = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    right.set_hexpand(true);
    right.set_valign(gtk4::Align::Center);
    let hdr = gtk4::Label::new(Some(tr("Base ambient light").as_str()));
    hdr.add_css_class("dim-label");
    hdr.add_css_class("heading");
    hdr.set_halign(gtk4::Align::Start);
    hdr.set_margin_start(4);
    right.append(&hdr);

    match led {
        None => {
            let g = adw::PreferencesGroup::new();
            g.add(&info_row(&tr("Status"), &tr("no base detected on this endpoint")));
            right.append(&g);
        }
        Some(led) => {
            let group = adw::PreferencesGroup::new();
            let brow = adw::ActionRow::builder()
                .title(tr("Brightness"))
                .subtitle(tr("Drag = live preview · Persist = save to the joystick"))
                .build();
            let sbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
            let scale = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, 0.0, 255.0, 1.0);
            scale.set_value(led.brightness.unwrap_or(0) as f64);
            scale.set_draw_value(false);
            scale.set_size_request(170, -1);
            scale.set_valign(gtk4::Align::Center);
            let val = gtk4::Label::new(Some(&format!("{} / 255", led.brightness.unwrap_or(0))));
            val.add_css_class("dim-label");
            val.add_css_class("numeric"); // chiffres tabulaires
            // Largeur FIXE (« 255 / 255 ») + alignée à droite : la valeur peut
            // passer de 1 à 3 chiffres sans déplacer le slider ni le bouton.
            val.set_width_chars(9);
            val.set_max_width_chars(9);
            val.set_xalign(1.0);
            // LIVE (fire-and-forget) pendant le drag — non persistant, sans confirm.
            {
                let path = path.clone();
                let val = val.clone();
                scale.connect_value_changed(move |s| {
                    let v = s.value() as u8;
                    val.set_text(&format!("{v} / 255"));
                    live_send(&path, device, family, v);
                });
            }
            let persist = gtk4::Button::with_label(&tr("Persist"));
            persist.add_css_class("suggested-action");
            persist.set_valign(gtk4::Align::Center);
            {
                let overlay = overlay.clone();
                let path = path.clone();
                let scale = scale.clone();
                persist.connect_clicked(move |_| {
                    let v = scale.value() as u8;
                    confirm_apply(
                        &overlay,
                        &tr("Persist the brightness?"),
                        &tr_f("Save brightness {} to the joystick?\n\nAutomatic backup first. Reversible.", &[v.to_string().as_str()]),
                        path.clone(),
                        device,
                        family,
                        BlWrite::Brightness(v),
                        Rc::new(|| {}),
                    );
                });
            }
            sbox.append(&scale);
            sbox.append(&val);
            sbox.append(&persist);
            brow.add_suffix(&sbox);
            group.add(&brow);
            right.append(&group);

            // Mode segmenté (ACTIVÉ) — chaque choix derrière confirmation.
            let mlabel = gtk4::Label::new(Some(tr("Mode").as_str()));
            mlabel.add_css_class("dim-label");
            mlabel.set_halign(gtk4::Align::Start);
            mlabel.set_margin_start(4);
            mlabel.set_margin_top(6);
            right.append(&mlabel);

            let active = if led.brightness.unwrap_or(0) == 0 {
                tr("Off")
            } else if led.breathing == Some(true) {
                tr("Breathing")
            } else {
                tr("Static")
            };
            let seg = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
            seg.add_css_class("linked");
            seg.set_homogeneous(true);
            for (label, wr) in [
                (tr("Static"), BlWrite::Breathing(false)),
                (tr("Breathing"), BlWrite::Breathing(true)),
                (tr("Off"), BlWrite::Brightness(0)),
            ] {
                let btn = gtk4::Button::with_label(&label);
                if label == active {
                    btn.add_css_class("wl-seg-active");
                }
                let overlay = overlay.clone();
                let path = path.clone();
                let seg2 = seg.clone();
                let btn2 = btn.clone();
                btn.connect_clicked(move |_| {
                    let seg = seg2.clone();
                    let btn = btn2.clone();
                    let on_ok: Rc<dyn Fn()> = Rc::new(move || {
                        let mut c = seg.first_child();
                        while let Some(w) = c {
                            w.remove_css_class("wl-seg-active");
                            c = w.next_sibling();
                        }
                        btn.add_css_class("wl-seg-active");
                    });
                    confirm_apply(
                        &overlay,
                        &tr("Change the lighting mode?"),
                        &tr_f("Write \"{}\"?\n\nAutomatic backup first. Reversible.", &[label.as_str()]),
                        path.clone(),
                        device,
                        family,
                        wr,
                        on_ok,
                    );
                });
                seg.append(&btn);
            }
            right.append(&seg);
        }
    }
    cols.append(&right);
    outer.append(&cols);

    let note = adw::PreferencesGroup::new();
    let nrow = adw::ActionRow::builder()
        .title(tr("Saving to the joystick"))
        .subtitle(tr("The live preview is not kept; \"Persist\" and the mode choice save it to the joystick, with an automatic backup."))
        .build();
    nrow.add_css_class("property");
    note.add(&nrow);
    outer.append(&note);

    adw::Clamp::builder().maximum_size(920).child(&outer).build().upcast()
}

/// Envoi LIVE de la luminosité (SET_LEDX 0x49) — non persistant, fire-and-forget.
fn live_send(path: &str, device: u8, family: u8, value: u8) {
    if let Ok(t) = HidrawTransport::open(path) {
        let _ = t.set_led(device, family, p::LED_INDEX_BACKLIGHT, value);
    }
}

/// Confirmation puis application (worker) d'une écriture flash de rétroéclairage.
#[allow(clippy::too_many_arguments)]
fn confirm_apply(
    overlay: &adw::ToastOverlay,
    heading: &str,
    body: &str,
    path: String,
    device: u8,
    family: u8,
    wr: BlWrite,
    on_ok: Rc<dyn Fn()>,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", &tr("Cancel"));
    dialog.add_response("apply", &tr("Write"));
    dialog.set_response_appearance("apply", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");

    let ov = overlay.clone();
    dialog.connect_response(None, move |_dlg, resp| {
        if resp != "apply" {
            return;
        }
        let ts = glib::DateTime::now_local()
            .ok()
            .and_then(|d| d.format("%Y%m%d-%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let path = path.clone();
        let (tx, rx) = async_channel::bounded::<Result<model::WriteOutcome, String>>(1);
        gio::spawn_blocking(move || {
            let res = HidrawTransport::open(&path).map_err(|e| e.to_string()).and_then(|mut t| {
                match wr {
                    BlWrite::Brightness(v) => {
                        model::set_backlight_persist(&mut t, device, family, v, false, Some(&ts))
                    }
                    BlWrite::Breathing(b) => {
                        model::set_breathing(&mut t, device, family, b, false, Some(&ts))
                    }
                }
                .map_err(|e| e.to_string())
            });
            let _ = tx.send_blocking(res);
        });
        let ov = ov.clone();
        let on_ok = on_ok.clone();
        glib::spawn_future_local(async move {
            if let Ok(res) = rx.recv().await {
                match res {
                    Ok(out) => {
                        let msg = if out.skipped {
                            tr("Already at this value — nothing to write")
                        } else if out.verified {
                            tr("Setting written")
                        } else if out.emitted {
                            tr("Written but not confirmed")
                        } else {
                            tr("Nothing written")
                        };
                        ov.add_toast(adw::Toast::new(&msg));
                        if out.verified || out.skipped {
                            on_ok();
                        }
                    }
                    Err(e) => ov.add_toast(adw::Toast::new(&tr_f("Failed: {}", &[e.as_str()]))),
                }
            }
        });
    });
    dialog.present(Some(overlay));
}

/// Photo de la base allumée, mise à l'échelle en largeur (~340 px).
fn base_picture() -> gtk4::Picture {
    let stream = gtk4::gio::MemoryInputStream::from_bytes(&gtk4::glib::Bytes::from_static(BASE_BL));
    let pic = gtk4::Picture::new();
    if let Ok(pb) = gtk4::gdk_pixbuf::Pixbuf::from_stream_at_scale(
        &stream,
        340,
        -1,
        true,
        gtk4::gio::Cancellable::NONE,
    ) {
        pic.set_paintable(Some(&gtk4::gdk::Texture::for_pixbuf(&pb)));
    }
    pic
}

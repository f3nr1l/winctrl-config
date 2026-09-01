//! Pages de configuration du manche sélectionné (une par onglet du ViewStack).
//!
//! **Discipline mono-écrivain** : aucune page ne lit le device elle-même. La
//! lecture est faite UNE fois, centralement (`ui::mod`), à chaque sélection de
//! manche ; le résultat est partagé (immuable) aux pages via [`PageState`]. Le
//! changement d'onglet ne relit donc jamais le matériel : jamais deux lectures
//! concurrentes sur le même endpoint.

use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::model::{self, Transport, WriteOutcome};
use crate::transport::HidrawTransport;

/// Instantané partagé (immuable) d'un manche : config + états annexes.
/// Défini côté `model` (lecture) ; s'étoffe au fil des pages.
pub use crate::model::DeviceSnapshot as Snapshot;

pub mod axes;
pub mod backlight;
pub mod buttons;
pub mod calibration;
pub mod main;
pub mod profiles;
pub mod remap;
pub mod vibration;

/// État courant propagé aux pages à chaque changement de sélection/lecture.
/// Toutes les variantes « avec manche » portent le `WinwingDevice` (les pages
/// locales, ex. Vibration, en ont besoin même quand la lecture a échoué).
#[derive(Debug, Clone)]
pub enum PageState {
    /// Aucun manche sélectionné.
    NoDevice,
    /// Lecture en cours pour ce manche.
    Loading(WinwingDevice),
    /// Lecture terminée : manche + données partagées.
    Ready(WinwingDevice, Rc<Snapshot>),
    /// Échec de lecture : manche + message prêt à afficher.
    Error(WinwingDevice, String),
}

impl PageState {
    /// Le manche concernée, s'il y en a une.
    pub fn device(&self) -> Option<&WinwingDevice> {
        match self {
            PageState::Loading(d) | PageState::Ready(d, _) | PageState::Error(d, _) => Some(d),
            PageState::NoDevice => None,
        }
    }
}

/// Une page de configuration. Reçoit l'état courant ; ne fait aucune I/O device.
pub trait Page {
    /// Identifiant stable dans le ViewStack.
    fn stack_id(&self) -> &'static str;
    /// Titre de l'onglet (traduit à terme via gettext).
    fn title(&self) -> &'static str;
    /// Nom d'icône symbolique de l'onglet.
    fn icon_name(&self) -> &'static str;
    /// Widget racine à insérer dans le ViewStack.
    fn root(&self) -> gtk4::Widget;
    /// Propage l'état courant (manche sélectionné / données lues).
    fn set_state(&self, state: PageState);
}

/// Construit toutes les pages, dans l'ordre d'affichage des onglets.
pub fn all_pages() -> Vec<Rc<dyn Page>> {
    vec![
        Rc::new(main::MainPage::new()),
        Rc::new(backlight::BacklightPage::new()),
        Rc::new(calibration::CalibrationPage::new()),
        Rc::new(vibration::VibrationPage::new()),
        Rc::new(profiles::ProfilesPage::new()),
        Rc::new(buttons::ButtonsPage::new()),
        Rc::new(remap::RemapPage::new()),
        Rc::new(axes::AxesPage::new()),
    ]
}

/// Widget de remplacement (`Adw.StatusPage`) pour une page pas encore faite.
pub fn stub(icon: &str, title: &str, desc: &str) -> gtk4::Widget {
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .description(desc)
        .build()
        .upcast()
}

// --- Petits helpers d'UI partagés entre pages -----------------------------

/// Ligne d'information simple (titre + valeur), valeurs échappées.
fn info_row(title: &str, value: &str) -> adw::ActionRow {
    adw::ActionRow::builder()
        .title(escape(title))
        .subtitle(escape(value))
        .build()
}

/// Texte centré (état vide / chargement / erreur) dans une zone verticale.
fn placeholder(content: &gtk4::Box, text: &str) {
    let label = gtk4::Label::new(Some(text));
    label.set_wrap(true);
    label.set_justify(gtk4::Justification::Center);
    label.add_css_class("dim-label");
    label.set_margin_top(48);
    label.set_margin_bottom(48);
    label.set_margin_start(24);
    label.set_margin_end(24);
    content.append(&label);
}

/// Vide un conteneur de tous ses enfants.
fn clear_box(content: &gtk4::Box) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
}

/// Échappe le markup Pango d'un texte dynamique.
fn escape(text: &str) -> String {
    gtk4::glib::markup_escape_text(text).to_string()
}

/// Zone verticale scrollable standard d'une page (contenu à remplir).
fn scroll_area() -> (gtk4::ScrolledWindow, gtk4::Box) {
    let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content.set_vexpand(true);
    let root = gtk4::ScrolledWindow::builder()
        .hscrollbar_policy(gtk4::PolicyType::Never)
        .vexpand(true)
        .child(&content)
        .build();
    (root, content)
}

// --- Écritures gardées, mutualisées entre pages ---------------------------

/// Une écriture flash déclenchable derrière confirmation. Toutes les variantes
/// sont `Copy` (données scalaires) → recopiables dans le handler de réponse.
#[derive(Clone, Copy)]
pub(super) enum WriteAction {
    /// Zone morte d'un axe de base (uint32) : `y_axis` sinon X.
    DeadzoneBase { y_axis: bool, value: u32 },
    /// Zone morte du twist Rz (0x104, octet 2).
    DeadzoneTwist(u8),
    /// Mode 4x32 de la base (0xC8) + redémarrage.
    FourX32(bool),
    /// Réinitialisation usine (0xB4 + restart).
    RestoreDefault,
}

impl WriteAction {
    fn run(
        self,
        t: &mut HidrawTransport,
        device: u8,
        family: u8,
        ts: &str,
    ) -> Result<WriteOutcome, crate::model::TransportError> {
        let ts = Some(ts);
        match self {
            WriteAction::DeadzoneBase { y_axis, value } => {
                model::set_deadzone_base(t, device, family, y_axis, value, false, ts)
            }
            WriteAction::DeadzoneTwist(v) => {
                model::set_deadzone_twist(t, device, family, v, false, ts)
            }
            WriteAction::FourX32(on) => model::set_4x32(t, device, family, on, true, false, ts),
            WriteAction::RestoreDefault => model::restore_default(t, device, family, false, ts),
        }
    }
}

/// Message de toast pour un résultat d'écriture.
fn write_result_msg(out: &WriteOutcome) -> String {
    if out.skipped {
        tr("Already at this value — nothing to write")
    } else if out.verified {
        tr("Written")
    } else if out.emitted {
        tr("Written but not confirmed")
    } else {
        tr("Nothing written")
    }
}

/// Confirmation (Adw.AlertDialog) puis application (worker) d'une [`WriteAction`],
/// avec backup+diff+écho+relecture côté modèle et toast de résultat.
#[allow(clippy::too_many_arguments)]
pub(super) fn confirm_write(
    overlay: &adw::ToastOverlay,
    heading: &str,
    body: &str,
    destructive: bool,
    path: String,
    device: u8,
    family: u8,
    action: WriteAction,
    on_ok: Rc<dyn Fn()>,
) {
    let dialog = adw::AlertDialog::new(Some(heading), Some(body));
    dialog.add_response("cancel", &tr("Cancel"));
    let apply_label = if destructive { tr("Reset") } else { tr("Write") };
    dialog.add_response("apply", &apply_label);
    dialog.set_response_appearance(
        "apply",
        if destructive {
            adw::ResponseAppearance::Destructive
        } else {
            adw::ResponseAppearance::Suggested
        },
    );
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
        let (tx, rx) = async_channel::bounded::<Result<WriteOutcome, String>>(1);
        gio::spawn_blocking(move || {
            let res = HidrawTransport::open(&path).map_err(|e| e.to_string()).and_then(|mut t| {
                action.run(&mut t, device, family, &ts).map_err(|e| e.to_string())
            });
            let _ = tx.send_blocking(res);
        });
        let ov = ov.clone();
        let on_ok = on_ok.clone();
        glib::spawn_future_local(async move {
            if let Ok(res) = rx.recv().await {
                match res {
                    Ok(out) => {
                        ov.add_toast(adw::Toast::new(&write_result_msg(&out)));
                        if out.verified || out.skipped {
                            on_ok();
                        }
                    }
                    Err(e) => ov.add_toast(adw::Toast::new(&tr_f("Failed: {}", &[&e]))),
                }
            }
        });
    });
    dialog.present(Some(overlay));
}

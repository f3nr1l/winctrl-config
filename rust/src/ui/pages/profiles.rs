//! Page « Profils » : profils de config **locaux** (fichiers JSON).
//!
//! - Liste les profils enregistrés sous `~/.local/share/winctrl/profiles/` et
//!   leurs détails (nom, date, appareil, nombre d'octets).
//! - **Capturer** : lit la config écrivable du manche (device READ, sûr) et
//!   l'écrit dans un FICHIER profil. Autorisé (aucune écriture device).
//! - **Appliquer** un profil = écriture dans le manche, non disponible pour l'instant.
//!
//! Format de fichier `winwing-profile` v1. Tout est autonome dans ce module.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::enumerate::{Controller, WinwingDevice};
use crate::i18n::{tr, tr_f};
use crate::model::Transport;
use crate::protocol as p;
use crate::transport::HidrawTransport;

use super::{clear_box, info_row, placeholder, scroll_area, Page, PageState};

const PROFILE_FORMAT: &str = "winwing-profile";
const PROFILE_VERSION: u32 = 1;
/// Fin de la région capturée : toute la config utile tient en 0x000–0x1dc.
const PROFILE_DUMP_END: u32 = 0x1DC;

// --- Format de fichier ----------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DeviceMeta {
    #[serde(default)]
    hidraw: String,
    #[serde(default)]
    pid: String,
    #[serde(default)]
    product_name: String,
    #[serde(default)]
    serial: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ProfileController {
    device: u8,
    family: u8,
    #[serde(default)]
    model: String,
    /// offset (« 0x00d8 ») -> octets hexa (« 01 ff ff ff »), ordonné.
    #[serde(default)]
    entries: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Profile {
    format: String,
    version: u32,
    name: String,
    #[serde(default)]
    created: String,
    #[serde(default)]
    app_version: String,
    device: DeviceMeta,
    controllers: Vec<ProfileController>,
}

impl Profile {
    fn is_valid(&self) -> bool {
        self.format == PROFILE_FORMAT
            && self.version <= PROFILE_VERSION
            && !self.controllers.is_empty()
    }

    /// Nombre total d'octets (offsets) capturés.
    fn entry_count(&self) -> usize {
        self.controllers.iter().map(|c| c.entries.len()).sum()
    }
}

// --- Capture (device READ -> fichier) -------------------------------------

/// Lit la config écrivable de chaque contrôleur (0x00–0x1dc, offsets profilables)
/// et bâtit un profil. **Read-only device** (uniquement des READ_CFG_DATA).
fn capture_device<T: Transport>(
    path: &str,
    controllers: &[Controller],
    name: &str,
    dev: &WinwingDevice,
) -> std::io::Result<Profile> {
    let mut t = T::open(path)?;
    let mut ctrls = Vec::new();
    for c in controllers {
        let mut entries = BTreeMap::new();
        let mut off = 0u32;
        while off <= PROFILE_DUMP_END {
            if p::is_profile_writable(off) {
                if let Some(v) = t.read_cfg(c.device, c.family, off) {
                    entries.insert(format!("0x{off:04x}"), p::hx(&v));
                }
            }
            off += 4;
        }
        ctrls.push(ProfileController {
            device: c.device,
            family: c.family,
            model: p::controller_name(c.device, c.family).unwrap_or("").to_string(),
            entries,
        });
    }
    t.close();
    let created = glib::DateTime::now_local()
        .ok()
        .and_then(|d| d.format("%Y-%m-%d %H:%M").ok())
        .map(|s| s.to_string())
        .unwrap_or_default();
    Ok(Profile {
        format: PROFILE_FORMAT.to_string(),
        version: PROFILE_VERSION,
        name: name.to_string(),
        created,
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        device: DeviceMeta {
            hidraw: dev.hidraw.clone(),
            pid: format!("{:04x}", dev.pid),
            product_name: dev.product.clone(),
            serial: dev.serial.clone(),
        },
        controllers: ctrls,
    })
}

// --- Persistance ----------------------------------------------------------

fn profiles_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/winctrl/profiles"))
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in name.trim().chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let s = out.trim_matches('-').to_string();
    if s.is_empty() {
        "profil".to_string()
    } else {
        s
    }
}

fn save_profile(prof: &Profile) -> std::io::Result<PathBuf> {
    let dir = profiles_dir()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "HOME introuvable"))?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.json", slug(&prof.name)));
    let json = serde_json::to_string_pretty(prof).map_err(std::io::Error::other)?;
    std::fs::write(&path, json)?;
    Ok(path)
}

/// `[(path, profil)]` des profils valides du dossier, triés par nom.
fn list_profiles() -> Vec<(PathBuf, Profile)> {
    let Some(dir) = profiles_dir() else {
        return Vec::new();
    };
    let mut out: Vec<(PathBuf, Profile)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            if let Ok(txt) = std::fs::read_to_string(&path) {
                if let Ok(prof) = serde_json::from_str::<Profile>(&txt) {
                    if prof.is_valid() {
                        out.push((path, prof));
                    }
                }
            }
        }
    }
    out.sort_by_key(|(_, prof)| prof.name.to_lowercase());
    out
}

// --- État + page ----------------------------------------------------------

struct ProfState {
    key: Option<String>,
    dev: Option<WinwingDevice>,
    profiles: Vec<(PathBuf, Profile)>,
}

pub struct ProfilesPage {
    root: gtk4::ScrolledWindow,
    content: gtk4::Box,
    state: Rc<RefCell<ProfState>>,
}

impl ProfilesPage {
    pub fn new() -> Self {
        let (root, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        ProfilesPage {
            root,
            content,
            state: Rc::new(RefCell::new(ProfState {
                key: None,
                dev: None,
                profiles: Vec::new(),
            })),
        }
    }
}

impl Default for ProfilesPage {
    fn default() -> Self {
        Self::new()
    }
}

fn device_key(dev: &WinwingDevice) -> String {
    format!("{:04x}:{}", dev.pid, dev.hidraw)
}

impl Page for ProfilesPage {
    fn stack_id(&self) -> &'static str {
        "profiles"
    }
    fn title(&self) -> &'static str {
        "Profiles"
    }
    fn icon_name(&self) -> &'static str {
        "document-save-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.root.clone().upcast()
    }
    fn set_state(&self, state: PageState) {
        match state.device() {
            None => {
                self.state.borrow_mut().key = None;
                clear_box(&self.content);
                placeholder(&self.content, &tr("Select a joystick from the list."));
            }
            Some(dev) => {
                let key = device_key(dev);
                if self.state.borrow().key.as_deref() == Some(key.as_str()) {
                    return;
                }
                {
                    let mut st = self.state.borrow_mut();
                    st.key = Some(key);
                    st.dev = Some(dev.clone());
                    st.profiles = list_profiles();
                }
                render(&self.content, &self.state);
            }
        }
    }
}

fn render(content: &gtk4::Box, state: &Rc<RefCell<ProfState>>) {
    clear_box(content);
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    vbox.set_margin_top(18);
    vbox.set_margin_bottom(18);

    // --- Capturer ------------------------------------------------------------
    let cap = adw::PreferencesGroup::new();
    cap.set_title(&tr("Capture the current configuration"));
    cap.set_description(Some(&tr(
        "Reads the joystick's writable configuration (identity excluded) and saves it to a profile file.",
    )));
    let name_row = adw::EntryRow::builder().title(tr("Profile name")).build();
    let cap_btn = gtk4::Button::with_label(&tr("Capture"));
    cap_btn.add_css_class("suggested-action");
    cap_btn.set_valign(gtk4::Align::Center);
    {
        let state = Rc::clone(state);
        let content = content.clone();
        let name_row = name_row.clone();
        let cap = cap.clone();
        cap_btn.connect_clicked(move |btn| {
            let name = name_row.text().to_string();
            let name = if name.trim().is_empty() {
                tr("profile")
            } else {
                name
            };
            let (path, controllers, dev) = {
                let st = state.borrow();
                match &st.dev {
                    Some(d) => (d.hidraw.clone(), d.controllers.clone(), d.clone()),
                    None => return,
                }
            };
            btn.set_sensitive(false);
            cap.set_description(Some(&tr("Reading the joystick…")));
            let (tx, rx) = async_channel::bounded::<std::io::Result<()>>(1);
            gio::spawn_blocking(move || {
                let res = capture_device::<HidrawTransport>(&path, &controllers, &name, &dev)
                    .and_then(|prof| save_profile(&prof).map(|_| ()));
                let _ = tx.send_blocking(res);
            });
            let state = Rc::clone(&state);
            let content = content.clone();
            let cap = cap.clone();
            let btn = btn.clone();
            glib::spawn_future_local(async move {
                if let Ok(res) = rx.recv().await {
                    match res {
                        Ok(()) => {
                            state.borrow_mut().profiles = list_profiles();
                            render(&content, &state);
                        }
                        Err(e) => {
                            cap.set_description(Some(&tr_f("Capture failed: {}", &[e.to_string().as_str()])));
                            btn.set_sensitive(true);
                        }
                    }
                }
            });
        });
    }
    name_row.add_suffix(&cap_btn);
    cap.add(&name_row);
    vbox.append(&cap);

    // --- Importer ------------------------------------------------------------
    let imp = adw::PreferencesGroup::new();
    imp.set_title(&tr("Import a profile"));
    imp.set_description(Some(&tr(
        "Loads a profile file (.json) into the local list. No write to the device.",
    )));
    let imp_row = adw::ActionRow::builder()
        .title(tr("From a file…"))
        .subtitle(tr("A profile exported from this app (winwing-profile format)"))
        .build();
    let imp_btn = gtk4::Button::with_label(&tr("Import"));
    imp_btn.set_valign(gtk4::Align::Center);
    {
        let state = Rc::clone(state);
        let content = content.clone();
        let imp = imp.clone();
        imp_btn.connect_clicked(move |_| {
            let dialog = gtk4::FileDialog::builder()
                .title(tr("Import a profile"))
                .build();
            let parent = content.root().and_downcast::<gtk4::Window>();
            let state = Rc::clone(&state);
            let content = content.clone();
            let imp = imp.clone();
            dialog.open(parent.as_ref(), gio::Cancellable::NONE, move |res| {
                let Ok(file) = res else { return };
                let Some(path) = file.path() else { return };
                let parsed = std::fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| serde_json::from_str::<Profile>(&t).ok());
                match parsed {
                    Some(prof) if prof.is_valid() => match save_profile(&prof) {
                        Ok(_) => {
                            state.borrow_mut().profiles = list_profiles();
                            render(&content, &state);
                        }
                        Err(e) => imp.set_description(Some(&tr_f("Import failed: {}", &[e.to_string().as_str()]))),
                    },
                    _ => imp.set_description(Some(&tr("Invalid file (not a winwing profile)"))),
                }
            });
        });
    }
    imp_row.add_suffix(&imp_btn);
    imp_row.set_activatable_widget(Some(&imp_btn));
    imp.add(&imp_row);
    vbox.append(&imp);

    // --- Profils enregistrés -------------------------------------------------
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Saved profiles"));
    let profiles = state.borrow().profiles.clone();
    if profiles.is_empty() {
        group.add(&info_row(
            &tr("No profile"),
            &tr("Capture the configuration to create the first one."),
        ));
    }
    for (path, prof) in &profiles {
        let subtitle = tr_f(
            "{} · {} · {} settings",
            &[
                if prof.created.is_empty() { "—" } else { &prof.created },
                if prof.device.product_name.is_empty() {
                    "?"
                } else {
                    &prof.device.product_name
                },
                prof.entry_count().to_string().as_str(),
            ],
        );
        let row = adw::ActionRow::builder()
            .title(super::escape(&prof.name))
            .subtitle(super::escape(&subtitle))
            .build();

        // Appliquer — non disponible pour l'instant (écriture dans le manche).
        let apply = gtk4::Button::with_label(&tr("Apply"));
        apply.set_sensitive(false);
        apply.set_valign(gtk4::Align::Center);
        apply.set_tooltip_text(Some(&tr("Applying a profile is not available yet")));
        row.add_suffix(&apply);

        // Exporter ce profil vers un fichier choisi (aucune écriture device).
        let export = gtk4::Button::from_icon_name("document-save-symbolic");
        export.add_css_class("flat");
        export.set_valign(gtk4::Align::Center);
        export.set_tooltip_text(Some(&tr("Export this profile to a file")));
        {
            let content = content.clone();
            let prof = prof.clone();
            export.connect_clicked(move |_| {
                let json = match serde_json::to_string_pretty(&prof) {
                    Ok(j) => j,
                    Err(_) => return,
                };
                let dialog = gtk4::FileDialog::builder()
                    .title(tr("Export the profile"))
                    .initial_name(format!("{}.json", slug(&prof.name)))
                    .build();
                let parent = content.root().and_downcast::<gtk4::Window>();
                dialog.save(parent.as_ref(), gio::Cancellable::NONE, move |res| {
                    if let Ok(file) = res {
                        if let Some(path) = file.path() {
                            let _ = std::fs::write(path, &json);
                        }
                    }
                });
            });
        }
        row.add_suffix(&export);

        // Supprimer le fichier local (aucune écriture device).
        let del = gtk4::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk4::Align::Center);
        del.set_tooltip_text(Some(&tr("Delete this profile file")));
        {
            let state = Rc::clone(state);
            let content = content.clone();
            let path = path.clone();
            del.connect_clicked(move |_| {
                let _ = std::fs::remove_file(&path);
                state.borrow_mut().profiles = list_profiles();
                render(&content, &state);
            });
        }
        row.add_suffix(&del);
        group.add(&row);
    }
    vbox.append(&group);

    // --- Note ----------------------------------------------------------------
    let note = adw::PreferencesGroup::new();
    let row = adw::ActionRow::builder()
        .title(tr("Applying not available"))
        .subtitle(tr("Capturing, importing and exporting profiles are available. Applying a profile to the joystick is not offered yet."))
        .build();
    row.add_css_class("property");
    note.add(&row);
    vbox.append(&note);

    let clamp = adw::Clamp::builder().maximum_size(760).child(&vbox).build();
    content.append(&clamp);
}

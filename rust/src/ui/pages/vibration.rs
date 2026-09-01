//! Page « Vibration » : test du moteur + éditeur de courbes d'effet **génériques**.
//!
//! Deux choses distinctes :
//! - **Tester** : commande le moteur directement (force feedback `FF_RUMBLE`,
//!   [`crate::rumble`]) à l'intensité choisie — pour *sentir* le moteur.
//! - **Effets** : des courbes de réponse `entrée 0–100 % → intensité 0–100 %`,
//!   génériques (aucun couplage à un simulateur). Un effet ne se déclenche qu'en
//!   jeu, piloté par une source externe (pont) ; la page ne fait que l'éditer et le
//!   persister (JSON sous `~/.local/share/winctrl/vibration/<clé>.json`).

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use serde::{Deserialize, Serialize};

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::rumble;
use crate::vibration as engine;

use super::{clear_box, placeholder, scroll_area, Page, PageState};

// --- Format de fichier (propre à l'app, ordonné) --------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Cv {
    #[serde(default)]
    c: f64,
    #[serde(default)]
    v: Vec<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Points {
    #[serde(default)]
    x: Vec<f64>,
    #[serde(default)]
    cv: Vec<Cv>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct VibEffect {
    name: String,
    #[serde(default)]
    enable: bool,
    #[serde(rename = "xAxis", default, skip_serializing_if = "Option::is_none")]
    x_axis: Option<String>,
    #[serde(rename = "xMin", default)]
    x_min: f64,
    #[serde(rename = "xMax", default = "one")]
    x_max: f64,
    #[serde(rename = "vMin", default)]
    v_min: f64,
    #[serde(rename = "vMax", default = "hundred")]
    v_max: f64,
    #[serde(default)]
    points: Points,
}

fn one() -> f64 {
    1.0
}
fn hundred() -> f64 {
    100.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct VibFile {
    #[serde(default = "fmt_tag")]
    format: String,
    #[serde(default = "one_u32")]
    version: u32,
    /// Intensité locale (0-100) — NE pilote PAS le moteur ici.
    #[serde(default = "fifty")]
    intensity: f64,
    #[serde(default)]
    effects: Vec<VibEffect>,
}

fn fmt_tag() -> String {
    "winwing-vibration".to_string()
}
fn one_u32() -> u32 {
    1
}
fn fifty() -> f64 {
    50.0
}

impl VibEffect {
    /// Points `v` de la 1re courbe `cv` (créée si absente), alignés sur `x`.
    fn v0(&mut self) -> &mut Vec<f64> {
        if self.points.cv.is_empty() {
            let v = vec![0.0; self.points.x.len()];
            self.points.cv.push(Cv { c: 0.0, v });
        }
        &mut self.points.cv[0].v
    }

    /// Garantit `len(v) == len(x)` sur chaque palier `cv` (tronque/complète).
    fn sanitize(&mut self) {
        let n = self.points.x.len();
        if self.points.cv.is_empty() {
            self.points.cv.push(Cv { c: 0.0, v: vec![0.0; n] });
        }
        for cv in &mut self.points.cv {
            cv.v.resize(n, 0.0);
        }
    }

    fn npoints(&self) -> usize {
        self.points.x.len()
    }

    /// Coordonnées `(x, v)` du point `i` (1re courbe), ou `None`.
    fn point_at(&self, i: usize) -> Option<(f64, f64)> {
        let x = *self.points.x.get(i)?;
        let v = self
            .points
            .cv
            .first()
            .and_then(|c| c.v.get(i).copied())
            .unwrap_or(0.0);
        Some((x, v))
    }

    /// Pose le point `i` à `(x, v)` (sur tous les paliers `cv`).
    fn set_point(&mut self, i: usize, x: f64, v: f64) {
        if let Some(px) = self.points.x.get_mut(i) {
            *px = x;
        }
        for cv in &mut self.points.cv {
            if let Some(pv) = cv.v.get_mut(i) {
                *pv = v;
            }
        }
    }

    fn add_point(&mut self) {
        let x = self.points.x.last().copied().unwrap_or(0.0) + 1.0;
        self.points.x.push(x);
        for cv in &mut self.points.cv {
            cv.v.push(0.0);
        }
        if self.points.cv.is_empty() {
            self.points.cv.push(Cv { c: 0.0, v: vec![0.0; self.points.x.len()] });
        }
    }

    fn remove_point(&mut self, i: usize) {
        if self.points.x.len() <= 1 {
            return; // on garde au moins un point
        }
        if i < self.points.x.len() {
            self.points.x.remove(i);
            for cv in &mut self.points.cv {
                if i < cv.v.len() {
                    cv.v.remove(i);
                }
            }
        }
    }
}

fn effect(name: &str, enable: bool, x_axis: Option<&str>, x_min: f64, x_max: f64,
          xs: &[f64], vs: &[f64]) -> VibEffect {
    VibEffect {
        name: name.to_string(),
        enable,
        x_axis: x_axis.map(str::to_string),
        x_min,
        x_max,
        v_min: 0.0,
        v_max: 100.0,
        points: Points { x: xs.to_vec(), cv: vec![Cv { c: 0.0, v: vs.to_vec() }] },
    }
}

/// Jeu de courbes par défaut, **génériques** : chaque effet mappe une entrée
/// 0–100 % (fournie par un jeu/pont externe) vers l'intensité moteur 0–100 %. Aucun
/// couplage à un simulateur particulier.
fn defaults() -> VibFile {
    VibFile {
        format: fmt_tag(),
        version: 1,
        intensity: 50.0,
        effects: vec![
            effect("Rising ramp", true, None, 0.0, 100.0,
                   &[0.0, 100.0], &[0.0, 100.0]),
            effect("Progressive", false, None, 0.0, 100.0,
                   &[0.0, 40.0, 70.0, 100.0], &[0.0, 8.0, 35.0, 100.0]),
            effect("Bell", false, None, 0.0, 100.0,
                   &[0.0, 25.0, 50.0, 75.0, 100.0], &[0.0, 60.0, 100.0, 60.0, 0.0]),
        ],
    }
}

/// Libellé d'un effet : les noms par défaut sont traduits, les noms personnalisés
/// passent tels quels.
fn effect_label(name: &str) -> String {
    tr(name)
}

// --- Persistance ----------------------------------------------------------

fn device_key(dev: &WinwingDevice) -> String {
    let mut k = format!("{:04x}", dev.pid);
    let serial: String = dev.serial.chars().filter(char::is_ascii_alphanumeric).collect();
    if !serial.is_empty() {
        k.push('-');
        k.push_str(&serial);
    }
    k
}

fn config_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".local/share/winctrl/vibration"))
}

fn config_path(key: &str) -> Option<PathBuf> {
    Some(config_dir()?.join(format!("{key}.json")))
}

fn load_config(key: &str) -> VibFile {
    let mut cfg = config_path(key)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| serde_json::from_str::<VibFile>(&s).ok())
        .unwrap_or_else(defaults);
    if cfg.effects.is_empty() {
        cfg = defaults();
    }
    for e in &mut cfg.effects {
        e.sanitize();
    }
    cfg
}

fn save_config(key: &str, cfg: &VibFile) -> std::io::Result<()> {
    let dir = config_dir().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::NotFound, "HOME introuvable")
    })?;
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{key}.json"));
    let json = serde_json::to_string_pretty(cfg).map_err(std::io::Error::other)?;
    std::fs::write(path, json)
}

// --- État de la page ------------------------------------------------------

struct VibState {
    key: Option<String>,
    evdev: Option<String>,
    config: VibFile,
    selected: usize,
    /// Effet `FF_RUMBLE` du bouton « Tester » en cours (Drop arrête le moteur).
    rumble: Option<crate::rumble::Rumble>,
}

pub struct VibrationPage {
    root: gtk4::ScrolledWindow,
    content: gtk4::Box,
    state: Rc<RefCell<VibState>>,
}

impl VibrationPage {
    pub fn new() -> Self {
        let (root, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        VibrationPage {
            root,
            content,
            state: Rc::new(RefCell::new(VibState {
                key: None,
                evdev: None,
                config: defaults(),
                selected: 0,
                rumble: None,
            })),
        }
    }
}

impl Default for VibrationPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for VibrationPage {
    fn stack_id(&self) -> &'static str {
        "vibration"
    }
    fn title(&self) -> &'static str {
        "Vibration"
    }
    // (visible strings translated via i18n)
    fn icon_name(&self) -> &'static str {
        "media-playback-start-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.root.clone().upcast()
    }
    fn set_state(&self, state: PageState) {
        match state.device() {
            None => {
                let mut st = self.state.borrow_mut();
                st.key = None;
                st.evdev = None;
                st.rumble = None; // arrête un test en cours
                drop(st);
                clear_box(&self.content);
                placeholder(&self.content, &tr("Select a joystick from the list."));
            }
            Some(dev) => {
                let key = device_key(dev);
                // Page LOCALE : ne recharge que si le manche change (préserve
                // l'édition en cours quand la lecture device se termine).
                if self.state.borrow().key.as_deref() == Some(key.as_str()) {
                    return;
                }
                {
                    let mut st = self.state.borrow_mut();
                    st.config = load_config(&key);
                    st.selected = 0;
                    st.key = Some(key);
                    st.evdev = Some(dev.evdev.clone());
                    st.rumble = None;
                }
                render(&self.content, &self.state);
            }
        }
    }
}

/// (Re)construit tout le contenu de la page à partir de l'état courant.
fn render(content: &gtk4::Box, state: &Rc<RefCell<VibState>>) {
    clear_box(content);

    // --- Aperçu de courbe (Cairo) --------------------------------------------
    let area = gtk4::DrawingArea::new();
    area.set_content_height(220);
    area.set_hexpand(true);
    area.set_margin_top(6);
    area.set_margin_bottom(6);
    area.set_margin_start(6);
    area.set_margin_end(6);
    {
        let state = Rc::clone(state);
        area.set_draw_func(move |a, cr, w, h| draw_curve(a, cr, w, h, &state.borrow()));
    }
    // Glisser un point de la courbe à la souris (met à jour x et intensité).
    {
        let drag = gtk4::GestureDrag::new();
        drag.set_button(gtk4::gdk::BUTTON_PRIMARY);
        let sel = Rc::new(Cell::new(None::<usize>));
        let start = Rc::new(Cell::new((0.0f64, 0.0f64)));
        {
            let (state, area, sel, start) =
                (Rc::clone(state), area.clone(), Rc::clone(&sel), Rc::clone(&start));
            drag.connect_drag_begin(move |_, x, y| {
                start.set((x, y));
                let idx = vib_hit_test(&state, &area, x, y);
                sel.set(idx);
                if let Some(i) = idx {
                    vib_set_point_from_pointer(&state, &area, i, x, y);
                    area.queue_draw();
                }
            });
        }
        {
            let (state, area, sel, start) =
                (Rc::clone(state), area.clone(), Rc::clone(&sel), Rc::clone(&start));
            drag.connect_drag_update(move |_, ox, oy| {
                if let Some(i) = sel.get() {
                    let (sx, sy) = start.get();
                    vib_set_point_from_pointer(&state, &area, i, sx + ox, sy + oy);
                    area.queue_draw();
                }
            });
        }
        {
            let (state, content, sel) = (Rc::clone(state), content.clone(), Rc::clone(&sel));
            drag.connect_drag_end(move |_, _, _| {
                if sel.get().is_some() {
                    render(&content, &state); // rafraîchit les champs X/V
                }
            });
        }
        area.add_controller(drag);
    }
    let frame = gtk4::Frame::new(None);
    frame.set_child(Some(&area));
    frame.set_margin_start(12);
    frame.set_margin_end(12);

    // --- Groupe « Effet » ----------------------------------------------------
    let group = adw::PreferencesGroup::new();
    group.set_title(&tr("Test and effects"));
    group.set_description(Some(&tr(
        "\"Test\" drives the motor directly. Effects are generic curves (0–100 % input → \
         intensity) driven in-game by an external source.",
    )));

    let labels: Vec<String> = state
        .borrow()
        .config
        .effects
        .iter()
        .map(|e| effect_label(&e.name))
        .collect();
    let model = gtk4::StringList::new(&labels.iter().map(String::as_str).collect::<Vec<_>>());
    let combo = adw::ComboRow::builder().title(tr("Effect")).model(&model).build();
    combo.set_selected(state.borrow().selected as u32);
    {
        let state = Rc::clone(state);
        let content = content.clone();
        combo.connect_selected_notify(move |row| {
            state.borrow_mut().selected = row.selected() as usize;
            render(&content, &state); // rebâtit points + aperçu pour l'effet choisi
        });
    }
    group.add(&combo);

    let sel = state.borrow().selected;
    let enabled = state.borrow().config.effects[sel].enable;
    let sw = adw::SwitchRow::builder().title(tr("Enabled")).active(enabled).build();
    {
        let state = Rc::clone(state);
        sw.connect_active_notify(move |r| {
            let sel = state.borrow().selected;
            state.borrow_mut().config.effects[sel].enable = r.is_active();
        });
    }
    group.add(&sw);

    // Intensité : sert au bouton « Tester » (commande directe du moteur).
    let intensity = state.borrow().config.intensity;
    let irow = adw::ActionRow::builder()
        .title(tr("Test intensity"))
        .subtitle(tr("Level of the \"Test\" button (0–100 %), also used as the global level"))
        .build();
    let scale = gtk4::Scale::with_range(gtk4::Orientation::Horizontal, 0.0, 100.0, 1.0);
    scale.set_value(intensity);
    scale.set_size_request(220, -1);
    scale.set_valign(gtk4::Align::Center);
    scale.set_draw_value(false);
    // Valeur en label de LARGEUR FIXE (« 100 ») + alignée à droite : de 1 à 3
    // chiffres, le slider et la mise en page ne bougent pas.
    let ival = gtk4::Label::new(Some(&format!("{}", intensity as i32)));
    ival.add_css_class("dim-label");
    ival.add_css_class("numeric");
    ival.set_width_chars(3);
    ival.set_max_width_chars(3);
    ival.set_xalign(1.0);
    {
        let state = Rc::clone(state);
        let ival = ival.clone();
        scale.connect_value_changed(move |s| {
            state.borrow_mut().config.intensity = s.value();
            ival.set_text(&format!("{}", s.value() as i32));
        });
    }
    let sbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 10);
    sbox.append(&scale);
    sbox.append(&ival);
    irow.add_suffix(&sbox);
    group.add(&irow);

    // Bouton « Tester » : commande le moteur directement (FF_RUMBLE), ~700 ms.
    let test_row = adw::ActionRow::builder()
        .title(tr("Test the motor"))
        .subtitle(tr("Direct pulse at the intensity above (force feedback) — reversible"))
        .build();
    let test_btn = gtk4::Button::builder()
        .label(tr("Test"))
        .valign(gtk4::Align::Center)
        .build();
    test_btn.add_css_class("suggested-action");
    test_row.add_suffix(&test_btn);
    test_row.set_activatable_widget(Some(&test_btn));
    {
        let state = Rc::clone(state);
        test_btn.connect_clicked(move |btn| {
            let (evdev, pct) = {
                let s = state.borrow();
                (s.evdev.clone(), s.config.intensity)
            };
            let Some(evdev) = evdev else {
                btn.set_tooltip_text(Some(&tr("Input device not found for this joystick")));
                return;
            };
            let mag = rumble::percent_to_magnitude(pct);
            match rumble::Rumble::upload(&evdev, mag, 700) {
                Ok(r) => {
                    let _ = r.play();
                    state.borrow_mut().rumble = Some(r);
                    btn.set_tooltip_text(None);
                    // Nettoie l'effet après la durée (le noyau a déjà stoppé le moteur).
                    let state2 = Rc::clone(&state);
                    glib::timeout_add_local_once(Duration::from_millis(850), move || {
                        state2.borrow_mut().rumble = None;
                    });
                }
                Err(e) => {
                    btn.set_tooltip_text(Some(&tr_f(
                        "Failed: {} — the input device must be writable (force feedback).",
                        &[e.to_string().as_str()],
                    )));
                }
            }
        });
    }
    group.add(&test_row);

    // --- Groupe « Points » ---------------------------------------------------
    let pts_group = adw::PreferencesGroup::new();
    pts_group.set_title(&tr("Curve points"));
    let (xs, vs) = {
        let st = state.borrow();
        let e = &st.config.effects[sel];
        (e.points.x.clone(), e.points.cv.first().map(|c| c.v.clone()).unwrap_or_default())
    };
    for (i, &xval) in xs.iter().enumerate() {
        let row = adw::ActionRow::builder()
            .title(tr_f("Point {}", &[(i + 1).to_string().as_str()]))
            .build();

        let x_spin = gtk4::SpinButton::with_range(-100000.0, 100000.0, 1.0);
        x_spin.set_digits(3);
        x_spin.set_value(xval);
        x_spin.set_valign(gtk4::Align::Center);
        {
            let state = Rc::clone(state);
            let area = area.clone();
            x_spin.connect_value_changed(move |s| {
                let sel = state.borrow().selected;
                let mut st = state.borrow_mut();
                if let Some(x) = st.config.effects[sel].points.x.get_mut(i) {
                    *x = s.value();
                }
                drop(st);
                area.queue_draw();
            });
        }

        let v_spin = gtk4::SpinButton::with_range(0.0, 1000.0, 1.0);
        v_spin.set_digits(2);
        v_spin.set_value(*vs.get(i).unwrap_or(&0.0));
        v_spin.set_valign(gtk4::Align::Center);
        {
            let state = Rc::clone(state);
            let area = area.clone();
            v_spin.connect_value_changed(move |s| {
                let sel = state.borrow().selected;
                let mut st = state.borrow_mut();
                if let Some(v) = st.config.effects[sel].v0().get_mut(i) {
                    *v = s.value();
                }
                drop(st);
                area.queue_draw();
            });
        }

        let del = gtk4::Button::from_icon_name("user-trash-symbolic");
        del.add_css_class("flat");
        del.set_valign(gtk4::Align::Center);
        del.set_tooltip_text(Some(&tr("Remove this point")));
        {
            let state = Rc::clone(state);
            let content = content.clone();
            del.connect_clicked(move |_| {
                let sel = state.borrow().selected;
                state.borrow_mut().config.effects[sel].remove_point(i);
                render(&content, &state);
            });
        }

        let xl = gtk4::Label::new(Some("X"));
        xl.add_css_class("dim-label");
        let vl = gtk4::Label::new(Some("V %"));
        vl.add_css_class("dim-label");
        row.add_suffix(&xl);
        row.add_suffix(&x_spin);
        row.add_suffix(&vl);
        row.add_suffix(&v_spin);
        row.add_suffix(&del);
        pts_group.add(&row);
    }

    // Ligne d'ajout de point.
    let add_row = adw::ActionRow::builder().title(tr("Add a point")).build();
    let add_btn = gtk4::Button::from_icon_name("list-add-symbolic");
    add_btn.add_css_class("flat");
    add_btn.set_valign(gtk4::Align::Center);
    {
        let state = Rc::clone(state);
        let content = content.clone();
        add_btn.connect_clicked(move |_| {
            let sel = state.borrow().selected;
            state.borrow_mut().config.effects[sel].add_point();
            render(&content, &state);
        });
    }
    add_row.add_suffix(&add_btn);
    add_row.set_activatable_widget(Some(&add_btn));
    pts_group.add(&add_row);

    // --- Bouton Enregistrer --------------------------------------------------
    let save_group = adw::PreferencesGroup::new();
    let save_row = adw::ActionRow::builder()
        .title(tr("Save the configuration"))
        .subtitle(tr("Saves the effects and intensity for this joystick"))
        .build();
    let save_btn = gtk4::Button::with_label(&tr("Save"));
    save_btn.add_css_class("suggested-action");
    save_btn.set_valign(gtk4::Align::Center);
    {
        let state = Rc::clone(state);
        let save_row = save_row.clone();
        save_btn.connect_clicked(move |_| {
            let st = state.borrow();
            let key = st.key.clone().unwrap_or_default();
            match save_config(&key, &st.config) {
                Ok(()) => save_row.set_subtitle(&tr("Saved")),
                Err(e) => save_row.set_subtitle(&tr_f("Failed: {}", &[e.to_string().as_str()])),
            }
        });
    }
    save_row.add_suffix(&save_btn);
    save_row.set_activatable_widget(Some(&save_btn));
    save_group.add(&save_row);

    // Assemblage : groupes + aperçu, centrés par un Clamp.
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 18);
    vbox.set_margin_top(18);
    vbox.set_margin_bottom(18);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    vbox.append(&group);
    vbox.append(&frame);
    vbox.append(&pts_group);
    vbox.append(&save_group);
    let clamp = adw::Clamp::builder().maximum_size(760).child(&vbox).build();
    content.append(&clamp);
}

/// Dessine l'aperçu de courbe de l'effet sélectionné.
/// Repère de tracé de l'effet courant. **Doit** coller au padding/plages de
/// [`draw_curve`] pour que le drag vise juste. `px(x) = pl + (x-xlo)/xspan*(pr-pl)`,
/// `py(v) = pb - (v-vlo)/vspan*(pb-pt)`.
struct Mapping {
    pl: f64,
    pr: f64,
    pt: f64,
    pb: f64,
    xlo: f64,
    xspan: f64,
    vlo: f64,
    vspan: f64,
}

fn vib_mapping(st: &VibState, w: f64, h: f64) -> Option<Mapping> {
    let e = st.config.effects.get(st.selected)?;
    if e.points.x.is_empty() {
        return None;
    }
    let xlo = e.points.x.iter().cloned().fold(f64::INFINITY, f64::min);
    let xhi = e.points.x.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let (vlo, vhi) = (e.v_min.min(e.v_max), e.v_min.max(e.v_max));
    let (lpad, rpad, tpad, bpad) = (44.0, 12.0, 12.0, 26.0);
    Some(Mapping {
        pl: lpad,
        pr: w - rpad,
        pt: tpad,
        pb: h - bpad,
        xlo,
        xspan: if xhi > xlo { xhi - xlo } else { 1.0 },
        vlo,
        vspan: if vhi > vlo { vhi - vlo } else { 1.0 },
    })
}

/// Index du point le plus proche du pointeur (dans un rayon de 18 px), ou `None`.
fn vib_hit_test(state: &Rc<RefCell<VibState>>, area: &gtk4::DrawingArea, px: f64, py: f64) -> Option<usize> {
    let st = state.borrow();
    let (w, h) = (area.width() as f64, area.height() as f64);
    let m = vib_mapping(&st, w, h)?;
    let e = st.config.effects.get(st.selected)?;
    let mut best = None;
    let mut best_d = 18.0_f64;
    for i in 0..e.npoints() {
        let (x, v) = e.point_at(i)?;
        let hx = m.pl + (x - m.xlo) / m.xspan * (m.pr - m.pl);
        let hy = m.pb - (v - m.vlo) / m.vspan * (m.pb - m.pt);
        let d = ((hx - px).powi(2) + (hy - py).powi(2)).sqrt();
        if d < best_d {
            best_d = d;
            best = Some(i);
        }
    }
    best
}

/// Pose le point `i` de l'effet courant depuis une position pointeur (coords widget),
/// borné au domaine `x` de l'effet et à la plage d'intensité.
fn vib_set_point_from_pointer(
    state: &Rc<RefCell<VibState>>,
    area: &gtk4::DrawingArea,
    i: usize,
    px: f64,
    py: f64,
) {
    let (w, h) = (area.width() as f64, area.height() as f64);
    let mapping = {
        let st = state.borrow();
        vib_mapping(&st, w, h)
    };
    let Some(m) = mapping else {
        return;
    };
    let mut st = state.borrow_mut();
    let sel = st.selected;
    if let Some(e) = st.config.effects.get_mut(sel) {
        let x = m.xlo + (px - m.pl) / (m.pr - m.pl) * m.xspan;
        let v = m.vlo + (m.pb - py) / (m.pb - m.pt) * m.vspan;
        let (xmn, xmx) = (e.x_min.min(e.x_max), e.x_min.max(e.x_max));
        e.set_point(i, x.clamp(xmn, xmx), v.clamp(m.vlo, m.vlo + m.vspan));
    }
}

fn draw_curve(area: &gtk4::DrawingArea, cr: &gtk4::cairo::Context, w: i32, h: i32, st: &VibState) {
    use std::f64::consts::TAU;
    let (w, h) = (w as f64, h as f64);
    // Couleur de premier plan du thème (s'adapte clair/sombre) pour grille+texte.
    let fg = area.color();
    let (fr, fgc, fb) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);
    // Accent bleu (identique dans les deux thèmes) pour la courbe et l'aire.
    let (ar, ag, ab) = (0.204, 0.518, 0.894); // #3584e4

    let (lpad, rpad, tpad, bpad) = (44.0, 12.0, 12.0, 26.0);
    let (pl, pr, pt, pb) = (lpad, w - rpad, tpad, h - bpad);

    // Grille (4×4).
    cr.set_line_width(1.0);
    for i in 0..=4 {
        let a = if i == 0 || i == 4 { 0.22 } else { 0.09 };
        cr.set_source_rgba(fr, fgc, fb, a);
        let x = pl + (pr - pl) * i as f64 / 4.0;
        cr.move_to(x, pt);
        cr.line_to(x, pb);
        let _ = cr.stroke();
        let y = pt + (pb - pt) * i as f64 / 4.0;
        cr.move_to(pl, y);
        cr.line_to(pr, y);
        let _ = cr.stroke();
    }

    let e = st.config.effects.get(st.selected);
    let vmax = e.map(|e| e.v_max).unwrap_or(100.0);
    let vmin = e.map(|e| e.v_min).unwrap_or(0.0);

    if let Some(e) = e {
        let vs0 = e.points.cv.first().map(|c| c.v.clone()).unwrap_or_default();
        let (sx, sv, anchors) = engine::sample_curve(&e.points.x, &vs0, e.v_min, e.v_max, 128);
        if sx.len() >= 2 {
            let xlo = sx.iter().cloned().fold(f64::INFINITY, f64::min);
            let xhi = sx.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
            let (vlo, vhi) = (vmin.min(vmax), vmin.max(vmax));
            let xspan = if xhi > xlo { xhi - xlo } else { 1.0 };
            let vspan = if vhi > vlo { vhi - vlo } else { 1.0 };
            let px = |x: f64| pl + (x - xlo) / xspan * (pr - pl);
            let py = |v: f64| pb - (v - vlo) / vspan * (pb - pt);

            // Aire dégradée sous la courbe.
            let grad = gtk4::cairo::LinearGradient::new(0.0, pt, 0.0, pb);
            grad.add_color_stop_rgba(0.0, ar, ag, ab, 0.18);
            grad.add_color_stop_rgba(1.0, ar, ag, ab, 0.02);
            cr.move_to(px(sx[0]), py(sv[0]));
            for i in 1..sx.len() {
                cr.line_to(px(sx[i]), py(sv[i]));
            }
            cr.line_to(px(sx[sx.len() - 1]), pb);
            cr.line_to(px(sx[0]), pb);
            cr.close_path();
            let _ = cr.set_source(&grad);
            let _ = cr.fill();

            // Courbe.
            cr.set_source_rgb(ar, ag, ab);
            cr.set_line_width(2.5);
            cr.move_to(px(sx[0]), py(sv[0]));
            for i in 1..sx.len() {
                cr.line_to(px(sx[i]), py(sv[i]));
            }
            let _ = cr.stroke();

            // Ancres : disque blanc, contour accent.
            for (x, v) in anchors {
                cr.arc(px(x), py(v), 4.0, 0.0, TAU);
                cr.set_source_rgb(1.0, 1.0, 1.0);
                let _ = cr.fill_preserve();
                cr.set_source_rgb(ar, ag, ab);
                cr.set_line_width(2.0);
                let _ = cr.stroke();
            }
        }
    }

    // Étiquettes d'axes (couleur fg atténuée).
    cr.set_source_rgba(fr, fgc, fb, 0.65);
    cr.set_font_size(10.0);
    cr.move_to(8.0, pt + 8.0);
    let _ = cr.show_text(&format!("{}", vmax as i32));
    cr.move_to(8.0, pb);
    let _ = cr.show_text(&format!("{}", vmin as i32));
    cr.move_to((pl + pr) / 2.0 - 70.0, h - 8.0);
    let _ = cr.show_text(&tr("Time / influence factor"));
}

//! Page « Remap » — réaffectation et répartition des boutons côté OS (uinput),
//! sans écriture dans le manche.
//!
//! Deux sessions **mutuellement exclusives** (une seule capture exclusive du
//! périphérique physique à la fois) :
//! - **Répartition 4×32** : expose les boutons en manettes virtuelles de 32
//!   boutons, pour contourner la limite de 32 boutons de certains jeux.
//! - **Réaffectation bouton par bouton** : table capture-pour-assigner, persistée
//!   par appareil ([`crate::remap_store`]).
//!
//! Le manche physique est capturé le temps de la session (le moniteur de l'onglet
//! Boutons est donc figé pendant ce temps). Réversible : désactiver rend la main.
//! La ré-émission est pompée par un timer GLib (~4 ms).

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f, trn_f};
use crate::livemon::LiveInput;
use crate::remap::{self, MAX_OUTPUT_ORDINAL};
use crate::remap_store::{self, RemapMapping};

use super::{clear_box, escape, placeholder, Page, PageState};

/// Cadence de pompage de la ré-émission pendant une session active.
const PUMP_INTERVAL: Duration = Duration::from_millis(4);

/// État interne mutable de la page.
struct Inner {
    overlay: adw::ToastOverlay,
    content: gtk4::Box,
    dev: Option<WinwingDevice>,
    mapping: RemapMapping,
    // Session active (split OU remap) : possède le grab + les devices virtuels.
    session: Option<remap::RemapSession>,
    session_source: Option<glib::SourceId>,
    session_mode: Option<&'static str>, // "split" | "remap"
    session_hidraw: Option<String>,
    // Références pour refléter l'état sans tout reconstruire.
    split_switch: Option<gtk4::Switch>,
    remap_switch: Option<gtk4::Switch>,
    split_row: Option<adw::ActionRow>,
    remap_row: Option<adw::ActionRow>,
}

pub struct RemapPage {
    overlay: adw::ToastOverlay,
    inner: Rc<RefCell<Inner>>,
    // Suspend les handlers de switch pendant un set_active programmatique.
    guard: Rc<Cell<bool>>,
}

impl RemapPage {
    pub fn new() -> Self {
        let content = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        content.set_vexpand(true);
        let scroll = gtk4::ScrolledWindow::builder()
            .hscrollbar_policy(gtk4::PolicyType::Never)
            .vexpand(true)
            .child(&content)
            .build();
        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&scroll));
        placeholder(&content, &tr("Select a joystick from the list."));
        let inner = Rc::new(RefCell::new(Inner {
            overlay: overlay.clone(),
            content,
            dev: None,
            mapping: RemapMapping::new(),
            session: None,
            session_source: None,
            session_mode: None,
            session_hidraw: None,
            split_switch: None,
            remap_switch: None,
            split_row: None,
            remap_row: None,
        }));
        RemapPage {
            overlay,
            inner,
            guard: Rc::new(Cell::new(false)),
        }
    }
}

impl Default for RemapPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for RemapPage {
    fn stack_id(&self) -> &'static str {
        "remap"
    }
    fn title(&self) -> &'static str {
        "Remap"
    }
    // (title source string is English; translated at display time)
    fn icon_name(&self) -> &'static str {
        "media-playlist-shuffle-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.overlay.clone().upcast()
    }

    fn set_state(&self, state: PageState) {
        let mut inner = self.inner.borrow_mut();
        match state {
            PageState::NoDevice => {
                teardown_session(&mut inner, true);
                inner.dev = None;
                inner.mapping = RemapMapping::new();
                reset_refs(&mut inner);
                clear_box(&inner.content);
                placeholder(&inner.content, &tr("Select a joystick from the list."));
            }
            PageState::Loading(dev) | PageState::Ready(dev, _) | PageState::Error(dev, _) => {
                // Changer de manche démonte une session en cours (grab sur l'ancienne).
                if inner.session_hidraw.as_deref() != Some(dev.hidraw.as_str()) {
                    teardown_session(&mut inner, true);
                }
                inner.mapping = remap_store::load_mapping(&dev, None);
                inner.dev = Some(dev);
                rebuild(&mut inner, &self.inner, &self.guard);
            }
        }
    }
}

/// Oublie les références de widgets (avant une reconstruction / vidage).
fn reset_refs(inner: &mut Inner) {
    inner.split_switch = None;
    inner.remap_switch = None;
    inner.split_row = None;
    inner.remap_row = None;
}

/// Démonte la session active : retire le pompage puis relâche le grab (Drop de
/// `RemapSession` = destroy des devices virtuels + ungrab + close). `remove_source`
/// = false quand on est DANS le callback de pompage (il se retire en renvoyant
/// `Break` ; le remove() ferait doublon).
fn teardown_session(inner: &mut Inner, remove_source: bool) {
    if remove_source {
        if let Some(s) = inner.session_source.take() {
            s.remove();
        }
    } else {
        inner.session_source = None;
    }
    inner.session = None;
    inner.session_mode = None;
    inner.session_hidraw = None;
}

/// (Re)construit la page depuis l'état courant (manche + mapping + session).
fn rebuild(inner: &mut Inner, inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    clear_box(&inner.content);
    reset_refs(inner);
    let Some(dev) = inner.dev.clone() else {
        placeholder(&inner.content, &tr("Select a joystick from the list."));
        return;
    };

    let page = adw::PreferencesPage::new();

    // --- Intro ------------------------------------------------------------
    let intro = adw::PreferencesGroup::builder()
        .title(tr("Button remapping and splitting"))
        .description(tr(
            "Remaps the buttons without writing anything to the joystick: a virtual controller \
             receives the remapped buttons, and the physical joystick is captured for the \
             duration of the session (the monitor in the Buttons tab is therefore frozen \
             during that time). Reversible. Splitting and remapping cannot both be active at \
             the same time.",
        ))
        .build();
    page.add(&intro);

    // --- Répartition 4×32 -------------------------------------------------
    let gsplit = adw::PreferencesGroup::builder()
        .title(tr("Splitting into 32-button controllers"))
        .description(tr(
            "Exposes the joystick's buttons on 2 virtual 32-button controllers, to work \
             around the 32-button limit of some games. No restart, no write to the joystick.",
        ))
        .build();
    let split_active = inner.session_mode == Some("split");
    let row_s = adw::ActionRow::builder()
        .title(tr("Enable splitting"))
        .build();
    let sw_s = gtk4::Switch::builder().valign(gtk4::Align::Center).build();
    if dev.evdev.is_empty() {
        sw_s.set_sensitive(false);
        row_s.set_subtitle(&tr("input device not found for this joystick"));
    } else {
        sw_s.set_active(split_active); // avant connect : pas de fire parasite
        row_s.set_subtitle(&split_subtitle(inner));
    }
    {
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        let d = dev.clone();
        sw_s.connect_active_notify(move |sw| {
            if g.get() {
                return;
            }
            if sw.is_active() {
                start_daemon(&ir, &g, &d, "split");
            } else if daemon_here(&ir, &d, "split") {
                stop_daemon(&ir, &g, &tr("Splitting stopped — joystick released"));
            }
        });
    }
    row_s.add_suffix(&sw_s);
    row_s.set_activatable_widget(Some(&sw_s));
    gsplit.add(&row_s);
    page.add(&gsplit);
    inner.split_switch = Some(sw_s);
    inner.split_row = Some(row_s);

    // --- Remap bouton par bouton ------------------------------------------
    let gremap = adw::PreferencesGroup::builder()
        .title(tr("Button-by-button remapping"))
        .description(tr(
            "Remaps each physical button to an output number. The joystick declares many \
             \"phantom\" buttons: \"Assign…\" captures the button actually pressed so they \
             don't have to be guessed.",
        ))
        .build();
    let remap_active = inner.session_mode == Some("remap");
    let row_r = adw::ActionRow::builder().title(tr("Enable remapping")).build();
    let sw_r = gtk4::Switch::builder().valign(gtk4::Align::Center).build();
    if dev.evdev.is_empty() {
        sw_r.set_sensitive(false);
        row_r.set_subtitle(&tr("input device not found for this joystick"));
    } else {
        sw_r.set_active(remap_active);
        row_r.set_subtitle(&remap_subtitle(inner));
    }
    {
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        let d = dev.clone();
        sw_r.connect_active_notify(move |sw| {
            if g.get() {
                return;
            }
            if sw.is_active() {
                start_daemon(&ir, &g, &d, "remap");
            } else if daemon_here(&ir, &d, "remap") {
                stop_daemon(&ir, &g, &tr("Remapping stopped — joystick released"));
            }
        });
    }
    row_r.add_suffix(&sw_r);
    row_r.set_activatable_widget(Some(&sw_r));
    gremap.add(&row_r);

    // Ligne d'ajout : capture (défaut) + saisie manuelle (repli).
    let assign = adw::ActionRow::builder()
        .title(tr("Remappings"))
        .subtitle(tr("Capture a physical button then choose its output number."))
        .build();
    let manual_btn = gtk4::Button::builder()
        .label(tr("Enter…"))
        .valign(gtk4::Align::Center)
        .css_classes(["flat"])
        .build();
    {
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        manual_btn.connect_clicked(move |_| remap_manual(&ir, &g));
    }
    let assign_btn = gtk4::Button::builder()
        .label(tr("Assign…"))
        .valign(gtk4::Align::Center)
        .css_classes(["suggested-action"])
        .build();
    {
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        assign_btn.connect_clicked(move |_| remap_assign(&ir, &g));
    }
    assign.add_suffix(&manual_btn);
    assign.add_suffix(&assign_btn);
    gremap.add(&assign);
    page.add(&gremap);
    inner.remap_switch = Some(sw_r);
    inner.remap_row = Some(row_r);

    // --- Table des réaffectations -----------------------------------------
    let glist = adw::PreferencesGroup::builder()
        .title(tr("Remapped buttons"))
        .build();
    if inner.mapping.is_empty() {
        glist.add(
            &adw::ActionRow::builder()
                .title(tr("No remapping"))
                .subtitle(tr("\"Assign…\" adds a button → output mapping"))
                .build(),
        );
    } else {
        let clear = gtk4::Button::builder()
            .label(tr("Clear all"))
            .valign(gtk4::Align::Center)
            .css_classes(["destructive-action", "flat"])
            .build();
        {
            let ir = Rc::clone(inner_rc);
            let g = Rc::clone(guard);
            let d = dev.clone();
            clear.connect_clicked(move |_| remap_clear_all(&ir, &g, &d));
        }
        glist.set_header_suffix(Some(&clear));
        for (src, dst) in inner.mapping.entries() {
            glist.add(&entry_row(inner_rc, guard, &dev, src, dst));
        }
    }
    page.add(&glist);

    inner.content.append(&page);
}

/// Une ligne de la table : bouton physique → sortie, avec bouton de retrait.
fn entry_row(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    dev: &WinwingDevice,
    src: u32,
    dst: u32,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(tr_f("Physical button #{}", &[src.to_string().as_str()]))
        .subtitle(tr_f("outputs as button #{}", &[dst.to_string().as_str()]))
        .build();
    let btn = gtk4::Button::builder()
        .icon_name("user-trash-symbolic")
        .valign(gtk4::Align::Center)
        .css_classes(["flat"])
        .tooltip_text(tr("Remove this remapping"))
        .build();
    {
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        let d = dev.clone();
        btn.connect_clicked(move |_| remap_remove(&ir, &g, &d, src));
    }
    row.add_suffix(&btn);
    row
}

// --- sous-titres d'état ----------------------------------------------------
fn split_subtitle(inner: &Inner) -> String {
    if inner.session_mode == Some("split") {
        let n = inner.session.as_ref().map_or(0, |s| s.n_slots()) as u32;
        trn_f(
            "active — {} 32-button controller",
            "active — {} 32-button controllers",
            n,
            &[n.to_string().as_str()],
        )
    } else {
        tr("inactive")
    }
}

fn remap_subtitle(inner: &Inner) -> String {
    let n = inner.mapping.len() as u32;
    if inner.session_mode == Some("remap") {
        trn_f("active — {} remapping", "active — {} remappings", n, &[n.to_string().as_str()])
    } else if n == 0 {
        tr("inactive — no remapping")
    } else {
        trn_f(
            "inactive — {} saved remapping",
            "inactive — {} saved remappings",
            n,
            &[n.to_string().as_str()],
        )
    }
}

// --- démons (split/remap partagent un seul grab) ---------------------------
/// Le démon actif porte-t-il CE manche dans CE mode ?
fn daemon_here(inner_rc: &Rc<RefCell<Inner>>, dev: &WinwingDevice, mode: &str) -> bool {
    let inner = inner_rc.borrow();
    inner.session_mode == Some(mode) && inner.session_hidraw.as_deref() == Some(dev.hidraw.as_str())
}

/// Démarre le split OU le remap. Un seul grab possible par device : tout démon déjà
/// actif est d'abord démonté (exclusion mutuelle).
fn start_daemon(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    dev: &WinwingDevice,
    mode: &'static str,
) {
    if dev.evdev.is_empty() {
        toast(inner_rc, &tr("Input device not found for this joystick"));
        refresh_switches(inner_rc, guard);
        return;
    }
    if mode == "remap" && inner_rc.borrow().mapping.is_empty() {
        toast(
            inner_rc,
            &tr("No remapping — add some before enabling it"),
        );
        refresh_switches(inner_rc, guard);
        return;
    }
    // exclusion mutuelle : relâcher le grab courant avant d'en prendre un autre.
    {
        let mut inner = inner_rc.borrow_mut();
        teardown_session(&mut inner, true);
    }
    let li = match LiveInput::open(&dev.evdev) {
        Ok(l) => l,
        Err(e) => {
            toast(inner_rc, &tr_f("Cannot open the input device: {}", &[e.to_string().as_str()]));
            refresh_switches(inner_rc, guard);
            return;
        }
    };
    let overrides = inner_rc.borrow().mapping.to_overrides();
    // Les courbes/inversions d'axe (onglet « Axes ») sont portées par TOUT démon :
    // splitter ou remapper n'empêche pas d'inverser le slider.
    let curves = crate::axis_store::load_curves(dev, None).to_curves();
    let plan = match remap::build_plan(&li, mode, &overrides, &curves) {
        Ok(p) => p,
        Err(e) => {
            toast(inner_rc, &tr_f("Invalid remap: {}", &[e.to_string().as_str()]));
            refresh_switches(inner_rc, guard);
            return;
        }
    };
    let n_slots = plan.n_slots();
    let mut sess = remap::RemapSession::new(li, plan);
    if let Err(e) = sess.start() {
        toast(
            inner_rc,
            &tr_f(
                "Virtual device unavailable: {}. Check that the \"uinput\" module is loaded \
                 and that the access rule is installed.",
                &[e.to_string().as_str()],
            ),
        );
        refresh_switches(inner_rc, guard);
        return;
    }
    {
        let mut inner = inner_rc.borrow_mut();
        inner.session = Some(sess);
        inner.session_mode = Some(mode);
        inner.session_hidraw = Some(dev.hidraw.clone());
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        let src = glib::source::timeout_add_local(PUMP_INTERVAL, move || on_pump(&ir, &g));
        inner.session_source = Some(src);
    }
    if mode == "split" {
        toast(
            inner_rc,
            &tr_f("Splitting active — {} virtual controllers", &[n_slots.to_string().as_str()]),
        );
    } else {
        toast(inner_rc, &tr("Remapping active — virtual controller"));
    }
    refresh_switches(inner_rc, guard);
}

fn stop_daemon(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>, message: &str) {
    {
        let mut inner = inner_rc.borrow_mut();
        teardown_session(&mut inner, true);
    }
    refresh_switches(inner_rc, guard);
    toast(inner_rc, message);
}

/// Tick de pompage : ré-émet les événements captés sur les devices virtuels.
fn on_pump(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) -> glib::ControlFlow {
    let gone = {
        let mut inner = inner_rc.borrow_mut();
        match inner.session.as_mut() {
            Some(s) => s.pump().is_err(), // Err = manche déconnecté (ENODEV…)
            None => return glib::ControlFlow::Break,
        }
    };
    if gone {
        {
            let mut inner = inner_rc.borrow_mut();
            teardown_session(&mut inner, false); // on EST la source : Break la retire
        }
        refresh_switches(inner_rc, guard);
        toast(inner_rc, &tr("Session stopped — joystick disconnected"));
        return glib::ControlFlow::Break;
    }
    glib::ControlFlow::Continue
}

/// Reflète l'état de la session sur les deux switches, sans redéclencher les
/// handlers (guard). Ne touche pas un switch insensible (garde « nœud introuvable »).
fn refresh_switches(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    let (split_sw, remap_sw, split_row, remap_row, split_active, remap_active, split_sub, remap_sub) = {
        let inner = inner_rc.borrow();
        (
            inner.split_switch.clone(),
            inner.remap_switch.clone(),
            inner.split_row.clone(),
            inner.remap_row.clone(),
            inner.session_mode == Some("split"),
            inner.session_mode == Some("remap"),
            split_subtitle(&inner),
            remap_subtitle(&inner),
        )
    };
    guard.set(true);
    if let Some(sw) = &split_sw {
        if sw.is_sensitive() {
            sw.set_active(split_active);
            if let Some(row) = &split_row {
                row.set_subtitle(&split_sub);
            }
        }
    }
    if let Some(sw) = &remap_sw {
        if sw.is_sensitive() {
            sw.set_active(remap_active);
            if let Some(row) = &remap_row {
                row.set_subtitle(&remap_sub);
            }
        }
    }
    guard.set(false);
}

fn toast(inner_rc: &Rc<RefCell<Inner>>, text: &str) {
    let overlay = inner_rc.borrow().overlay.clone();
    overlay.add_toast(adw::Toast::new(text));
}

fn parent_window(inner_rc: &Rc<RefCell<Inner>>) -> Option<gtk4::Window> {
    inner_rc.borrow().overlay.root().and_downcast::<gtk4::Window>()
}

// --- édition de la table (capture / saisie / retrait) ----------------------
/// Flux capture-pour-assigner : l'utilisateur presse un bouton physique, on
/// l'identifie par son ordinal live, puis on demande le numéro de sortie. Exige
/// qu'aucun démon ne tourne (le grab figerait le moniteur).
fn remap_assign(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    if inner_rc.borrow().session.is_some() {
        toast(inner_rc, &tr("Stop splitting or remapping before assigning a button"));
        return;
    }
    let Some(dev) = inner_rc.borrow().dev.clone() else {
        return;
    };
    if dev.evdev.is_empty() {
        toast(inner_rc, &tr("Input device not found — use \"Enter…\""));
        return;
    }
    let mut li = match LiveInput::open(&dev.evdev) {
        Ok(l) => l,
        Err(_) => {
            toast(inner_rc, &tr("Monitor unavailable — use \"Enter…\""));
            return;
        }
    };
    if li.state.buttons.is_empty() {
        toast(inner_rc, &tr("No button detected — use \"Enter…\""));
        return;
    }
    li.poll();
    let baseline: HashSet<u16> = li.state.pressed.clone();
    let li = Rc::new(RefCell::new(li));
    let captured = Rc::new(Cell::new(None::<u16>));
    let parent = parent_window(inner_rc);

    let dlg = adw::MessageDialog::new(
        parent.as_ref(),
        Some(&tr("Assign a physical button")),
        Some(&tr("Press on the joystick the physical button to remap.")),
    );
    let lbl = gtk4::Label::new(Some(tr("Waiting for a press…").as_str()));
    lbl.add_css_class("dim-label");
    dlg.set_extra_child(Some(&lbl));
    dlg.add_response("cancel", &tr("Cancel"));
    dlg.add_response("next", &tr("Next"));
    dlg.set_response_appearance("next", adw::ResponseAppearance::Suggested);
    dlg.set_response_enabled("next", false);
    dlg.set_close_response("cancel");

    // Sonde périodique : au premier appui nouveau, capture l'ordinal et active « Suivant ».
    let tick_li = Rc::clone(&li);
    let tick_cap = Rc::clone(&captured);
    let tick_lbl = lbl.clone();
    let tick_dlg = dlg.clone();
    let src = glib::source::timeout_add_local(Duration::from_millis(50), move || {
        let mut l = tick_li.borrow_mut();
        l.poll();
        if tick_cap.get().is_none() {
            let mut best: Option<(i32, u16)> = None;
            for &code in l.state.pressed.iter() {
                if baseline.contains(&code) {
                    continue;
                }
                let ord = l.state.button_index(code);
                if ord >= 1 && best.map_or(true, |(b, _)| ord < b) {
                    best = Some((ord, code));
                }
            }
            if let Some((ord, code)) = best {
                tick_cap.set(Some(code));
                tick_lbl.set_text(&tr_f("Physical button #{} detected", &[ord.to_string().as_str()]));
                tick_dlg.set_response_enabled("next", true);
            }
        }
        glib::ControlFlow::Continue
    });
    let src = Rc::new(Cell::new(Some(src)));

    let ir = Rc::clone(inner_rc);
    let g = Rc::clone(guard);
    let resp_li = Rc::clone(&li);
    let resp_cap = Rc::clone(&captured);
    let resp_src = Rc::clone(&src);
    let resp_dev = dev.clone();
    dlg.connect_response(None, move |_dlg, resp| {
        if let Some(s) = resp_src.take() {
            s.remove();
        }
        if resp != "next" {
            return;
        }
        let Some(code) = resp_cap.get() else {
            return;
        };
        let ord = resp_li.borrow().state.button_index(code);
        if ord >= 1 {
            remap_choose_output(&ir, &g, &resp_dev, ord as u32);
        }
    });
    dlg.present();
}

/// Repli sans capture : saisir l'ordinal source puis le numéro de sortie.
fn remap_manual(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    let Some(dev) = inner_rc.borrow().dev.clone() else {
        return;
    };
    // Nombre de boutons pour borner la saisie (ioctl : marche même sous grab).
    let n = LiveInput::open(&dev.evdev)
        .map(|l| l.state.buttons.len().max(1) as u32)
        .unwrap_or(MAX_OUTPUT_ORDINAL as u32);
    let ir = Rc::clone(inner_rc);
    let g = Rc::clone(guard);
    let d = dev.clone();
    number_dialog(
        parent_window(inner_rc),
        &tr("Source physical button"),
        &tr("Number (1-based) of the physical button to remap."),
        1,
        n,
        1,
        move |src| remap_choose_output(&ir, &g, &d, src),
    );
}

fn remap_choose_output(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    dev: &WinwingDevice,
    src: u32,
) {
    let current = inner_rc.borrow().mapping.output_for(src);
    let ir = Rc::clone(inner_rc);
    let g = Rc::clone(guard);
    let d = dev.clone();
    number_dialog(
        parent_window(inner_rc),
        &tr("Output number"),
        &tr_f("Physical button #{} will output as button no.:", &[src.to_string().as_str()]),
        1,
        MAX_OUTPUT_ORDINAL as u32,
        current,
        move |dst| remap_set(&ir, &g, &d, src, dst),
    );
}

fn remap_set(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    dev: &WinwingDevice,
    src: u32,
    dst: u32,
) {
    if let Err(e) = inner_rc.borrow_mut().mapping.set(src, dst) {
        toast(inner_rc, &tr_f("Invalid remapping: {}", &[e.to_string().as_str()]));
        return;
    }
    let mapping = inner_rc.borrow().mapping.clone();
    if let Err(e) = remap_store::save_mapping(dev, &mapping, None) {
        toast(inner_rc, &tr_f("Cannot save: {}", &[e.to_string().as_str()]));
        return;
    }
    if src == dst {
        toast(
            inner_rc,
            &tr_f("Remapping of button #{} removed (identity)", &[src.to_string().as_str()]),
        );
    } else {
        toast(inner_rc, &tr_f("Button #{} → #{} saved", &[src.to_string().as_str(), dst.to_string().as_str()]));
        if inner_rc.borrow().session_mode == Some("remap") {
            toast(inner_rc, &tr("Restart remapping to apply it"));
        }
    }
    let mut inner = inner_rc.borrow_mut();
    rebuild(&mut inner, inner_rc, guard);
}

fn remap_remove(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    dev: &WinwingDevice,
    src: u32,
) {
    inner_rc.borrow_mut().mapping.clear(src);
    let mapping = inner_rc.borrow().mapping.clone();
    if let Err(e) = remap_store::save_mapping(dev, &mapping, None) {
        toast(inner_rc, &tr_f("Cannot save: {}", &[e.to_string().as_str()]));
        return;
    }
    toast(inner_rc, &tr_f("Remapping of button #{} removed", &[src.to_string().as_str()]));
    if inner_rc.borrow().session_mode == Some("remap") {
        toast(inner_rc, &tr("Restart remapping to apply it"));
    }
    let mut inner = inner_rc.borrow_mut();
    rebuild(&mut inner, inner_rc, guard);
}

fn remap_clear_all(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>, dev: &WinwingDevice) {
    let dlg = adw::MessageDialog::new(
        parent_window(inner_rc).as_ref(),
        Some(&tr("Clear all?")),
        Some(&tr(
            "All remappings for this joystick will be deleted. No effect on the joystick \
             (OS-side remapping).",
        )),
    );
    dlg.add_response("cancel", &tr("Cancel"));
    dlg.add_response("clear", &tr("Clear all"));
    dlg.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
    dlg.set_close_response("cancel");
    let ir = Rc::clone(inner_rc);
    let g = Rc::clone(guard);
    let d = dev.clone();
    dlg.connect_response(None, move |_dlg, resp| {
        if resp != "clear" {
            return;
        }
        ir.borrow_mut().mapping.clear_all();
        let mapping = ir.borrow().mapping.clone();
        if let Err(e) = remap_store::save_mapping(&d, &mapping, None) {
            toast(&ir, &tr_f("Cannot save: {}", &[e.to_string().as_str()]));
            return;
        }
        toast(&ir, &tr("Remappings cleared"));
        let mut inner = ir.borrow_mut();
        rebuild(&mut inner, &ir, &g);
    });
    dlg.present();
}

/// Petit dialogue « nombre » (SpinButton) → appelle `on_ok(valeur)` sur validation.
fn number_dialog<F: Fn(u32) + 'static>(
    parent: Option<gtk4::Window>,
    heading: &str,
    body: &str,
    min: u32,
    max: u32,
    current: u32,
    on_ok: F,
) {
    let dlg = adw::MessageDialog::new(parent.as_ref(), Some(heading), Some(&escape(body)));
    let spin = gtk4::SpinButton::with_range(min as f64, max.max(min) as f64, 1.0);
    spin.set_value(current as f64);
    spin.set_valign(gtk4::Align::Center);
    spin.set_margin_top(6);
    spin.set_margin_bottom(6);
    dlg.set_extra_child(Some(&spin));
    dlg.add_response("cancel", &tr("Cancel"));
    dlg.add_response("ok", &tr("OK"));
    dlg.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
    dlg.set_default_response(Some("ok"));
    dlg.set_close_response("cancel");
    let spin2 = spin.clone();
    dlg.connect_response(None, move |_dlg, resp| {
        if resp == "ok" {
            on_ok(spin2.value().round() as u32);
        }
    });
    dlg.present();
}

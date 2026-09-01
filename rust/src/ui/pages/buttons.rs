//! Page « Boutons » — moniteur d'entrées live (axes + boutons) via evdev.
//!
//! Le « Test » de SimApp Pro : la valeur des axes (barres) et l'état des boutons
//! (pastilles) en direct. Ne lit PAS la trame vendor — on ouvre le nœud
//! `/dev/input/eventN` du joystick ([`crate::livemon`]) et on le sonde par un
//! réveil périodique GLib (~30 Hz, `timeout_add_local`) : lecture non bloquante
//! drainée à chaque tick, repeinte seulement si l'état a changé.
//!
//! Le nœud evdev du manche est porté par [`PageState`] (`WinwingDevice.evdev`) :
//! aucune ré-énumération, aucune I/O device côté page.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::livemon::LiveInput;

use super::{clear_box, placeholder, scroll_area, Page, PageState};

/// État interne mutable de la page (fd live + widgets à repeindre).
struct Inner {
    content: gtk4::Box,
    live: Option<LiveInput>,
    source: Option<glib::SourceId>,
    /// code ABS -> (barre de niveau, étiquette de valeur).
    axis_widgets: HashMap<u16, (gtk4::LevelBar, gtk4::Label)>,
    /// code evdev bouton -> pastille.
    button_widgets: HashMap<u16, gtk4::Label>,
}

pub struct ButtonsPage {
    root: gtk4::ScrolledWindow,
    inner: Rc<RefCell<Inner>>,
}

impl ButtonsPage {
    pub fn new() -> Self {
        let (root, content) = scroll_area();
        placeholder(&content, &tr("Select a joystick from the list."));
        let inner = Rc::new(RefCell::new(Inner {
            content,
            live: None,
            source: None,
            axis_widgets: HashMap::new(),
            button_widgets: HashMap::new(),
        }));
        ButtonsPage { root, inner }
    }
}

impl Default for ButtonsPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for ButtonsPage {
    fn stack_id(&self) -> &'static str {
        "buttons"
    }
    fn title(&self) -> &'static str {
        "Buttons"
    }
    fn icon_name(&self) -> &'static str {
        "input-gaming-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.root.clone().upcast()
    }

    fn set_state(&self, state: PageState) {
        let inner_rc = Rc::clone(&self.inner);
        let mut inner = self.inner.borrow_mut();
        teardown(&mut inner);
        clear_box(&inner.content);

        match state {
            PageState::NoDevice => {
                placeholder(&inner.content, &tr("Select a joystick from the list."));
            }
            PageState::Loading(dev) => {
                placeholder(&inner.content, &tr_f("Opening {} …", &[dev.hidraw.as_str()]));
            }
            PageState::Error(_dev, msg) => {
                placeholder(
                    &inner.content,
                    &tr_f("Joystick unavailable: {}\n(is the access rule installed?)", &[msg.as_str()]),
                );
            }
            PageState::Ready(dev, _snap) => {
                open_monitor(&mut inner, &inner_rc, &dev);
            }
        }
    }
}

/// Ferme le moniteur courant : retire la surveillance fd AVANT de fermer le fd,
/// puis oublie les widgets.
fn teardown(inner: &mut Inner) {
    if let Some(src) = inner.source.take() {
        src.remove();
    }
    inner.live = None; // Drop du LiveInput -> close(fd)
    inner.axis_widgets.clear();
    inner.button_widgets.clear();
}

/// Ouvre le nœud evdev du manche et installe la surveillance live.
/// `inner.content` est déjà vidé.
fn open_monitor(inner: &mut Inner, inner_rc: &Rc<RefCell<Inner>>, dev: &WinwingDevice) {
    if dev.evdev.is_empty() {
        placeholder(
            &inner.content,
            &tr("No input device associated with this joystick."),
        );
        return;
    }
    if !dev.live_readable() {
        placeholder(
            &inner.content,
            &tr_f(
                "Input device not readable: {}\n(install the access rule)",
                &[dev.evdev.as_str()],
            ),
        );
        return;
    }
    let li = match LiveInput::open(&dev.evdev) {
        Ok(li) => li,
        Err(e) => {
            placeholder(&inner.content, &tr_f("Monitor unavailable: {}", &[e.to_string().as_str()]));
            return;
        }
    };
    build_widgets(inner, &li);
    paint(inner, &li);
    inner.live = Some(li);

    let watched = Rc::clone(inner_rc);
    let src = glib::source::timeout_add_local(std::time::Duration::from_millis(33), move || {
        on_tick(&watched)
    });
    inner.source = Some(src);
}

/// Tick périodique : draine le nœud (non bloquant) et repeint si l'état a changé.
fn on_tick(inner_rc: &Rc<RefCell<Inner>>) -> glib::ControlFlow {
    let mut inner = inner_rc.borrow_mut();
    let changed = match inner.live.as_mut() {
        Some(li) => li.poll(),
        None => return glib::ControlFlow::Break,
    };
    if changed {
        let inner = &*inner;
        if let Some(li) = inner.live.as_ref() {
            paint(inner, li);
        }
    }
    glib::ControlFlow::Continue
}

/// Peint l'état courant (barres d'axes + pastilles) depuis `li`.
fn paint(inner: &Inner, li: &LiveInput) {
    for ax in &li.state.axes {
        if let Some((bar, val)) = inner.axis_widgets.get(&ax.code) {
            bar.set_value(ax.fraction());
            val.set_text(&ax.display());
        }
    }
    for (code, w) in &inner.button_widgets {
        if li.state.pressed.contains(code) {
            w.add_css_class("pressed");
        } else {
            w.remove_css_class("pressed");
        }
    }
}

/// Construit les groupes Axes + Boutons dans `inner.content` et remplit les tables
/// de widgets. `li` fournit les axes/boutons découverts.
fn build_widgets(inner: &mut Inner, li: &LiveInput) {
    let page = adw::PreferencesPage::new();

    // --- Axes : grille ALIGNÉE, barres de longueur FIXE ------------------
    // Colonnes : libellé (colonne alignée) | barre (piste 240 px, ne se dilate
    // pas) | valeur (96 px, alignée à droite). Ainsi 3 axes ou 8 axes ont des
    // barres IDENTIQUES et alignées, quelle que soit la longueur des libellés.
    let gaxes = adw::PreferencesGroup::new();
    gaxes.set_title(&tr("Axes"));
    if li.state.axes.is_empty() {
        gaxes.add(&adw::ActionRow::builder().title(tr("No axis detected")).build());
    } else {
        let grid = gtk4::Grid::new();
        grid.set_row_spacing(12);
        grid.set_column_spacing(14);
        grid.set_margin_top(12);
        grid.set_margin_bottom(12);
        grid.set_margin_start(14);
        grid.set_margin_end(14);
        grid.set_halign(gtk4::Align::Start);
        for (i, ax) in li.state.axes.iter().enumerate() {
            let title = if ax.desc.is_empty() {
                ax.name.clone()
            } else {
                format!("{} · {}", ax.name, tr(ax.desc))
            };
            let name = gtk4::Label::new(Some(&title));
            name.set_halign(gtk4::Align::Start);
            name.set_xalign(0.0);
            if ax.centered {
                name.set_tooltip_text(Some(&tr("self-centering axis (rests at center)")));
            }
            let bar = gtk4::LevelBar::builder()
                .min_value(0.0)
                .max_value(1.0)
                .value(ax.fraction())
                .valign(gtk4::Align::Center)
                .build();
            bar.add_css_class("winwing-axisbar");
            bar.set_hexpand(false);
            bar.set_size_request(240, 10);
            let value = gtk4::Label::new(Some(&ax.display()));
            value.add_css_class("dim-label");
            value.add_css_class("numeric");
            value.set_halign(gtk4::Align::End);
            value.set_xalign(1.0);
            value.set_size_request(96, -1);
            let r = i as i32;
            grid.attach(&name, 0, r, 1, 1);
            grid.attach(&bar, 1, r, 1, 1);
            grid.attach(&value, 2, r, 1, 1);
            inner.axis_widgets.insert(ax.code, (bar, value));
        }
        let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        card.add_css_class("card");
        card.append(&grid);
        gaxes.add(&card);
    }
    page.add(&gaxes);

    // --- Boutons ----------------------------------------------------------
    let gbtn = adw::PreferencesGroup::new();
    gbtn.set_title(&tr("Buttons"));
    let flow = gtk4::FlowBox::builder()
        .selection_mode(gtk4::SelectionMode::None)
        .max_children_per_line(16)
        .min_children_per_line(8)
        .column_spacing(4)
        .row_spacing(4)
        .margin_top(6)
        .margin_bottom(6)
        .homogeneous(true)
        .build();
    if li.state.buttons.is_empty() {
        let lbl = gtk4::Label::new(Some(tr("no button detected").as_str()));
        lbl.add_css_class("dim-label");
        flow.insert(&lbl, -1);
    }
    for (i, &code) in li.state.buttons.iter().enumerate() {
        let lbl = gtk4::Label::new(Some(&(i + 1).to_string()));
        lbl.add_css_class("winwing-btn");
        lbl.set_tooltip_text(Some(&tr_f("button {}", &[(i + 1).to_string().as_str()])));
        flow.insert(&lbl, -1);
        inner.button_widgets.insert(code, lbl);
    }
    gbtn.add(&flow);
    page.add(&gbtn);

    inner.content.append(&page);
}

// Le style des pastilles/barres du moniteur live (.winwing-btn/.pressed/
// .winwing-axisbar) est désormais dans le provider unique `ui::style`.

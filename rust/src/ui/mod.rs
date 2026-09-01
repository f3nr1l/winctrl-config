//! UI GTK4 / libadwaita — fenêtre de configuration **lecture seule** multi-pages.
//!
//! Structure : `Adw.Application` → `Adw.ApplicationWindow` +
//! `Adw.NavigationSplitView` (barre latérale = liste des manches | contenu =
//! `Adw.ViewStack` d'onglets, bascule `Adw.ViewSwitcher`). Toute écriture device
//! passe par une confirmation explicite avec sauvegarde préalable.
//!
//! **Discipline mono-écrivain (centralisée)** : à chaque sélection de manche,
//! UNE seule lecture (`model::read_device`) tourne sur un worker
//! (`gio::spawn_blocking`), jamais sur le thread UI ; le résultat est partagé
//! (immuable) aux pages via [`pages::PageState`]. Changer d'onglet ne relit
//! jamais le matériel → jamais deux lectures concurrentes sur un endpoint.

pub mod pages;
#[cfg(feature = "screenshot")]
mod screenshot;
mod style;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4::prelude::*;
use gtk4::{gio, glib};
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::enumerate::{self, WinwingDevice};
use crate::i18n::tr;
use crate::model::{self, DeviceSnapshot};
use crate::transport::HidrawTransport;

use pages::{Page, PageState};

/// Ré-exporté pour que le binaire n'ait pas à dépendre directement de `glib`.
pub use glib::ExitCode;

const APP_ID: &str = "io.github.f3nr1l.WinctrlConfig";

/// Point d'entrée de l'application GUI. Rend le code de sortie du process.
pub fn run() -> glib::ExitCode {
    crate::i18n::init();
    crate::model::migrate_legacy_data_dir();
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);
    app.run_with_args::<&str>(&[])
}

fn build_ui(app: &adw::Application) {
    // Mono-fenêtre : une activation supplémentaire (relance du lanceur, second
    // clic, ou activation D-Bus d'une instance déjà lancée) présente la fenêtre
    // existante au lieu d'en ouvrir une nouvelle.
    if let Some(win) = app.active_window() {
        win.present();
        return;
    }
    style::install();
    // Le Shell associe l'icône du lanceur (`<app-id>.png`) à la fenêtre par ce
    // nom d'icône (cohérent avec l'app-id / StartupWMClass).
    gtk4::Window::set_default_icon_name(APP_ID);
    // Vérif headless uniquement : force le schéma clair/sombre pour la capture.
    // En production l'app reste THÈME-AWARE (schéma Default, suit le système).
    #[cfg(feature = "screenshot")]
    match std::env::var("WINWING_SCHEME").as_deref() {
        Ok("dark") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceDark),
        Ok("light") => adw::StyleManager::default().set_color_scheme(adw::ColorScheme::ForceLight),
        _ => {}
    }
    let devices = Rc::new(enumerate::discover());
    let ui_pages: Rc<Vec<Rc<dyn Page>>> = Rc::new(pages::all_pages());
    let state = Rc::new(RefCell::new(PageState::NoDevice));
    // Compteur de génération : ignore le résultat d'une lecture rendue obsolète
    // par une sélection plus récente.
    let generation = Rc::new(Cell::new(0u64));

    // --- Pile de pages + bascule ---------------------------------------------
    let stack = adw::ViewStack::new();
    for p in ui_pages.iter() {
        stack.add_titled_with_icon(&p.root(), Some(p.stack_id()), &tr(p.title()), p.icon_name());
    }
    stack.set_vexpand(true);

    let switcher = adw::ViewSwitcher::builder()
        .stack(&stack)
        .policy(adw::ViewSwitcherPolicy::Wide)
        .build();
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&switcher));
    // Menu principal (avec « À propos »).
    let menu = gio::Menu::new();
    menu.append(Some(&tr("About WinCtrl Config")), Some("app.about"));
    let menu_btn = gtk4::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text(tr("Main menu"))
        .primary(true)
        .build();
    header.pack_end(&menu_btn);
    // Pastille d'état « Connecté » (verte) si au moins un manche accessible.
    let connected = devices.iter().any(|d| d.readable());
    let status = if connected { tr("Connected") } else { tr("Disconnected") };
    header.pack_end(&pill(
        &status,
        if connected { "success" } else { "neutral" },
    ));

    let banner = adw::Banner::new(&tr("Every write is confirmed before it is sent — automatic backup"));
    banner.set_revealed(true);

    let content_inner = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content_inner.append(&banner);
    content_inner.append(&stack);

    let content_toolbar = adw::ToolbarView::new();
    content_toolbar.add_top_bar(&header);
    content_toolbar.set_content(Some(&content_inner));
    // Bascule secondaire en bas, pour les fenêtres étroites.
    let switcher_bar = adw::ViewSwitcherBar::builder().stack(&stack).build();
    content_toolbar.add_bottom_bar(&switcher_bar);
    let content_page = adw::NavigationPage::new(&content_toolbar, "URSA MINOR");

    // Re-pousse l'état courant à l'onglet qui devient visible (aucune relecture).
    {
        let ui_pages = Rc::clone(&ui_pages);
        let state = Rc::clone(&state);
        stack.connect_visible_child_notify(move |st| {
            refresh_visible(st, &ui_pages, &state);
        });
    }

    // --- Barre latérale : liste des manches ----------------------------------
    let list = gtk4::ListBox::new();
    list.set_selection_mode(gtk4::SelectionMode::Single);
    list.add_css_class("navigation-sidebar");
    for dev in devices.iter() {
        list.append(&device_row(dev));
    }
    {
        let devices = Rc::clone(&devices);
        let stack = stack.clone();
        let ui_pages = Rc::clone(&ui_pages);
        let state = Rc::clone(&state);
        let generation = Rc::clone(&generation);
        let selected = Rc::new(RefCell::new(None::<String>));
        list.connect_row_selected(move |_, row| {
            let Some(row) = row else { return };
            let idx = row.index();
            if idx < 0 {
                return;
            }
            select_device(
                &devices,
                idx as usize,
                &stack,
                &ui_pages,
                &state,
                &generation,
                &selected,
            );
        });
    }

    let sidebar_toolbar = adw::ToolbarView::new();
    let sidebar_header = adw::HeaderBar::new();
    sidebar_header.set_title_widget(Some(&brand_widget()));
    sidebar_toolbar.add_top_bar(&sidebar_header);
    // Contenu : liste (extensible) + pied « ● Connexion directe ».
    let sb_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let scroller = gtk4::ScrolledWindow::builder().child(&list).vexpand(true).build();
    sb_box.append(&scroller);
    let footer = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    footer.set_margin_start(14);
    footer.set_margin_end(14);
    footer.set_margin_top(8);
    footer.set_margin_bottom(10);
    let dot = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    dot.add_css_class("wl-dot");
    dot.set_valign(gtk4::Align::Center);
    let foot = gtk4::Label::new(Some(tr("Direct connection").as_str()));
    foot.add_css_class("dim-label");
    foot.add_css_class("caption");
    footer.append(&dot);
    footer.append(&foot);
    sb_box.append(&footer);
    sidebar_toolbar.set_content(Some(&sb_box));
    let sidebar_page = adw::NavigationPage::new(&sidebar_toolbar, &tr("Joysticks"));

    // --- Split view + fenêtre ------------------------------------------------
    let split = adw::NavigationSplitView::new();
    split.set_sidebar(Some(&sidebar_page));
    split.set_content(Some(&content_page));
    split.set_min_sidebar_width(250.0);

    // Hauteur par défaut ; la vérif headless (feature screenshot) peut l'agrandir
    // pour montrer un onglet déplié en entier. Aucun effet en usage normal.
    #[allow(unused_mut)]
    let mut win_h: i32 = 760;
    #[cfg(feature = "screenshot")]
    if let Some(h) = std::env::var("WINWING_WIN_H").ok().and_then(|s| s.parse().ok()) {
        win_h = h;
    }
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("WinCtrl Config")
        .default_width(1200)
        .default_height(win_h)
        .content(&split)
        .build();

    // Action « À propos » (référencée par le menu principal `app.about`).
    let about = gio::SimpleAction::new("about", None);
    {
        let window = window.clone();
        about.connect_activate(move |_, _| present_about(&window));
    }
    app.add_action(&about);

    if let Some(first) = list.row_at_index(0) {
        list.select_row(Some(&first));
    }

    // Vérif headless : ouvre directement un onglet donné (feature screenshot).
    #[cfg(feature = "screenshot")]
    if let Ok(pg) = std::env::var("WINWING_PAGE") {
        stack.set_visible_child_name(&pg);
    }

    window.present();

    #[cfg(feature = "screenshot")]
    screenshot::arm(&window, app);
}

/// Sélectionne un manche : lance UNE lecture (worker) et diffuse l'état.
/// `selected` déduplique par endpoint : le double `row-selected` du démarrage
/// (select_row + re-émission au mapping) ne lance qu'une lecture.
#[allow(clippy::too_many_arguments)]
fn select_device(
    devices: &Rc<Vec<WinwingDevice>>,
    idx: usize,
    stack: &adw::ViewStack,
    ui_pages: &Rc<Vec<Rc<dyn Page>>>,
    state: &Rc<RefCell<PageState>>,
    generation: &Rc<Cell<u64>>,
    selected: &Rc<RefCell<Option<String>>>,
) {
    let Some(dev) = devices.get(idx).cloned() else {
        return;
    };
    if selected.borrow().as_deref() == Some(dev.hidraw.as_str()) {
        return; // déjà sélectionné : pas de relecture (évite la double-lecture)
    }
    *selected.borrow_mut() = Some(dev.hidraw.clone());

    let g = generation.get().wrapping_add(1);
    generation.set(g);
    *state.borrow_mut() = PageState::Loading(dev.clone());
    refresh_visible(stack, ui_pages, state);

    let path = dev.hidraw.clone();
    let controllers = dev.controllers.clone();
    let (tx, rx) = async_channel::bounded::<std::io::Result<DeviceSnapshot>>(1);
    // Worker : I/O hidraw bloquante, HORS thread UI. UNE lecture par endpoint.
    gio::spawn_blocking(move || {
        let _ = tx.send_blocking(model::read_snapshot::<HidrawTransport>(&path, &controllers));
    });

    let stack = stack.clone();
    let ui_pages = Rc::clone(ui_pages);
    let state = Rc::clone(state);
    let generation = Rc::clone(generation);
    glib::spawn_future_local(async move {
        if let Ok(res) = rx.recv().await {
            if generation.get() != g {
                return; // sélection obsolète : résultat ignoré
            }
            *state.borrow_mut() = match res {
                Ok(snap) => PageState::Ready(dev.clone(), Rc::new(snap)),
                Err(e) => PageState::Error(dev.clone(), format!("{} : {e}", dev.hidraw)),
            };
            refresh_visible(&stack, &ui_pages, &state);
        }
    });
}

/// Pousse l'état courant à la SEULE page visible (aucune I/O).
fn refresh_visible(stack: &adw::ViewStack, ui_pages: &[Rc<dyn Page>], state: &Rc<RefCell<PageState>>) {
    let Some(name) = stack.visible_child_name() else {
        return;
    };
    if let Some(page) = ui_pages.iter().find(|p| p.stack_id() == name.as_str()) {
        page.set_state(state.borrow().clone());
    }
}

/// Une ligne de la barre latérale décrivant un endpoint (manche + contrôleurs).
fn device_row(dev: &WinwingDevice) -> adw::ActionRow {
    // Nom commercial neutre : côté de la poignée si présente, sinon la base.
    let title = dev
        .controllers
        .iter()
        .find(|c| c.family == crate::protocol::FAMILY_GRIP)
        .or_else(|| dev.controllers.first())
        .map(|c| crate::i18n::tr(&crate::protocol::commercial_name(c.model())))
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| dev.product.clone());
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&title))
        .subtitle(format!("{}  ({:04x}:{:04x})", dev.hidraw, dev.vid, dev.pid))
        .build();
    if !dev.readable() {
        let warn = gtk4::Image::from_icon_name("dialog-warning-symbolic");
        warn.add_css_class("dim-label");
        warn.set_tooltip_text(Some(&tr("Joystick not accessible — install the access rule")));
        row.add_suffix(&warn);
    }
    row
}

/// Texte anglais source affiché dans la fenêtre « À propos » : disclaimer,
/// crédits et non-affiliation (les marques citées appartiennent à leurs détenteurs).
const ABOUT_COMMENTS: &str = "Third-party configuration tool for WinWing URSA MINOR joysticks.\n\n\
Unofficial project: not affiliated with, nor endorsed by WinWing. \"WINWING\", \"WINCTRL\" and \
\"URSA MINOR\" are trademarks of their respective owners, mentioned for identification only.\n\n\
Provided \"as is\", without any warranty. Writes to the hardware memory are performed at the \
user's own risk.";

/// Fenêtre « À propos » : nom, version, licence GPLv3+ (avec le texte complet),
/// disclaimer, crédits et mention de non-affiliation.
fn present_about(parent: &impl IsA<gtk4::Window>) {
    let about = adw::AboutWindow::builder()
        .application_name("WinCtrl Config")
        .application_icon(APP_ID)
        .version(env!("CARGO_PKG_VERSION"))
        .license_type(gtk4::License::Gpl30)
        .comments(tr(ABOUT_COMMENTS))
        .transient_for(parent)
        .build();
    about.present();
}

/// Pastille d'état stylée (classe `wl-pill` + variante).
fn pill(text: &str, kind: &str) -> gtk4::Label {
    let l = gtk4::Label::new(Some(text));
    l.add_css_class("wl-pill");
    l.add_css_class(kind);
    l.set_valign(gtk4::Align::Center);
    l
}

/// Logo carré accent + « WinCtrl Config » pour l'en-tête de la barre latérale.
fn brand_widget() -> gtk4::Box {
    let b = gtk4::Box::new(gtk4::Orientation::Horizontal, 9);
    let logo = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    logo.add_css_class("wl-logo");
    logo.set_valign(gtk4::Align::Center);
    let img = gtk4::Image::from_icon_name("input-gaming-symbolic");
    img.set_pixel_size(14);
    img.set_halign(gtk4::Align::Center);
    img.set_valign(gtk4::Align::Center);
    img.set_hexpand(true);
    img.set_vexpand(true);
    logo.append(&img);
    let name = gtk4::Label::new(Some("WinCtrl Config"));
    name.add_css_class("heading");
    b.append(&logo);
    b.append(&name);
    b
}

//! Page « Axes » — inversion et courbe de réponse par axe, côté OS (uinput).
//!
//! L'inversion et la courbe de réponse ne sont pas écrites dans le manche : elles
//! sont appliquées par une manette virtuelle (uinput). Le manche physique est
//! capturé, ses axes transformés, puis ré-émis sur le périphérique virtuel.
//! L'opération est donc réversible et n'écrit rien dans le matériel.
//!
//! L'édition (par axe) est persistée par appareil ([`crate::axis_store`]). Les
//! courbes s'appliquent dès qu'une session tourne : soit la répartition ou la
//! réaffectation de l'onglet « Remap » (qui les portent aussi), soit
//! l'interrupteur « Appliquer » de cette page (mode courbe seule — boutons à
//! l'identique). Une seule capture exclusive par manche : les sessions sont donc
//! mutuellement exclusives.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use std::collections::HashMap;

use crate::axis_curve::{CurveData, CurveType};
use crate::axis_store;
use crate::enumerate::WinwingDevice;
use crate::i18n::{tr, tr_f};
use crate::livemon::{AxisState, LiveInput, ABS_RX, ABS_RY, ABS_X, ABS_Y};
use crate::remap;

use super::{clear_box, escape, placeholder, Page, PageState};

const PUMP_INTERVAL: Duration = Duration::from_millis(4);
const ACCENT: (f64, f64, f64) = (0.204, 0.518, 0.894); // #3584e4

/// Couples d'axes qui peuvent tourner ensemble : `(primaire, partenaire, groupe)`.
/// Le contrôle de rotation n'est montré que sur le **primaire** ; il pose l'angle
/// et le groupe sur les deux axes. Groupe = identifiant `rotate_group` (SimApp).
const ROT_PAIRS: [(u16, u16, u32); 2] = [(ABS_X, ABS_Y, 1), (ABS_RX, ABS_RY, 2)];

/// Un axe éditable : sa description live + la cellule de sa courbe (partagée entre
/// contrôles, aperçu et persistance).
struct AxisCell {
    code: u16,
    curve: Rc<Cell<CurveData>>,
}

struct Inner {
    overlay: adw::ToastOverlay,
    content: gtk4::Box,
    dev: Option<WinwingDevice>,
    cells: Vec<AxisCell>,
    // Démon « courbe seule » : possède le grab + le device virtuel.
    session: Option<remap::RemapSession>,
    session_source: Option<glib::SourceId>,
    activate_switch: Option<gtk4::Switch>,
}

pub struct AxesPage {
    overlay: adw::ToastOverlay,
    inner: Rc<RefCell<Inner>>,
    guard: Rc<Cell<bool>>,
}

impl AxesPage {
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
        placeholder(&content, "Sélectionnez un manche dans la liste.");
        let inner = Rc::new(RefCell::new(Inner {
            overlay: overlay.clone(),
            content,
            dev: None,
            cells: Vec::new(),
            session: None,
            session_source: None,
            activate_switch: None,
        }));
        AxesPage {
            overlay,
            inner,
            guard: Rc::new(Cell::new(false)),
        }
    }
}

impl Default for AxesPage {
    fn default() -> Self {
        Self::new()
    }
}

impl Page for AxesPage {
    fn stack_id(&self) -> &'static str {
        "axes"
    }
    fn title(&self) -> &'static str {
        "Axes"
    }
    fn icon_name(&self) -> &'static str {
        "input-gaming-symbolic"
    }
    fn root(&self) -> gtk4::Widget {
        self.overlay.clone().upcast()
    }
    // (visible strings translated via i18n; source is English)

    fn set_state(&self, state: PageState) {
        {
            let mut inner = self.inner.borrow_mut();
            teardown_session(&mut inner, true);
            inner.cells.clear();
            inner.activate_switch = None;
            clear_box(&inner.content);
        }
        match state {
            PageState::NoDevice => {
                let mut inner = self.inner.borrow_mut();
                inner.dev = None;
                placeholder(&inner.content, &tr("Select a joystick from the list."));
            }
            PageState::Loading(dev) => {
                let inner = self.inner.borrow();
                placeholder(&inner.content, &tr_f("Reading {} …", &[dev.hidraw.as_str()]));
            }
            PageState::Error(_dev, msg) => {
                let inner = self.inner.borrow();
                placeholder(&inner.content, &tr_f("Read failed: {}", &[msg.as_str()]));
            }
            PageState::Ready(dev, _snap) => {
                self.inner.borrow_mut().dev = Some(dev.clone());
                build_content(&self.inner, &self.guard, &dev);
            }
        }
    }
}

/// (Re)construit tout le contenu depuis l'appareil courant.
fn build_content(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>, dev: &WinwingDevice) {
    let content = inner_rc.borrow().content.clone();
    clear_box(&content);

    if dev.evdev.is_empty() {
        placeholder(&content, &tr("Input device not found for this joystick."));
        return;
    }
    // Ouverture NON exclusive (pas de grab) : on lit juste la liste des axes.
    let axes: Vec<AxisState> = match LiveInput::open(&dev.evdev) {
        Ok(li) => li.state.axes.clone(),
        Err(e) => {
            placeholder(&content, &tr_f("Cannot open the input device: {}", &[e.to_string().as_str()]));
            return;
        }
    };
    if axes.is_empty() {
        placeholder(&content, &tr("No axis detected on this joystick."));
        return;
    }

    let stored = axis_store::load_curves(dev, None);
    // Réinitialise le registre des cellules pour ce device.
    inner_rc.borrow_mut().cells.clear();

    let page = adw::PreferencesPage::new();

    // --- Bandeau explicatif ---------------------------------------------------
    let intro = adw::PreferencesGroup::new();
    intro.set_title(&tr("Inversion and response curve"));
    intro.set_description(Some(&tr(
        "Settings applied by a virtual controller, never written to the joystick. Active \
         while a session is running: \"Apply\" below, or splitting or remapping in the \
         \"Remap\" tab.",
    )));
    page.add(&intro);

    // --- Activation -----------------------------------------------------------
    let act_group = adw::PreferencesGroup::new();
    let act_row = adw::ActionRow::builder()
        .title(tr("Apply (virtual controller)"))
        .subtitle(tr("Buttons unchanged, axes inverted and curved. Freezes the monitor in the \"Buttons\" tab."))
        .build();
    let act_switch = gtk4::Switch::builder().valign(gtk4::Align::Center).build();
    act_row.add_suffix(&act_switch);
    act_row.set_activatable_widget(Some(&act_switch));
    {
        let inner_rc = Rc::clone(inner_rc);
        let guard = Rc::clone(guard);
        act_switch.connect_active_notify(move |sw| {
            if guard.get() {
                return;
            }
            let dev = inner_rc.borrow().dev.clone();
            let Some(dev) = dev else { return };
            if sw.is_active() {
                start_daemon(&inner_rc, &guard, &dev);
            } else {
                stop_daemon(&inner_rc, &guard, &tr("Application stopped — axes returned to the joystick"));
            }
        });
    }
    act_group.add(&act_row);
    page.add(&act_group);
    inner_rc.borrow_mut().activate_switch = Some(act_switch);

    // --- Un groupe par axe ----------------------------------------------------
    // Pré-crée toutes les cellules : une rotation de couple (montrée sur l'axe
    // primaire) doit pouvoir écrire dans la cellule du partenaire.
    let cell_map: HashMap<u16, Rc<Cell<CurveData>>> = axes
        .iter()
        .map(|ax| (ax.code, Rc::new(Cell::new(stored.get(ax.code)))))
        .collect();
    {
        let mut inner = inner_rc.borrow_mut();
        for ax in &axes {
            inner.cells.push(AxisCell {
                code: ax.code,
                curve: Rc::clone(&cell_map[&ax.code]),
            });
        }
    }

    let axes_group = adw::PreferencesGroup::new();
    axes_group.set_title("Axes");
    for ax in &axes {
        let cell = Rc::clone(&cell_map[&ax.code]);
        axes_group.add(&build_axis_row(inner_rc, guard, ax, cell, &cell_map));
    }
    page.add(&axes_group);

    content.append(&page);
    refresh_switch(inner_rc, guard);
}

/// Ligne dépliable d'un axe : inversion + forme + deadzones + courbure + gain +
/// aperçu Cairo. Chaque contrôle met à jour la cellule, persiste, ré-applique si
/// un démon tourne, et redessine l'aperçu.
fn build_axis_row(
    inner_rc: &Rc<RefCell<Inner>>,
    guard: &Rc<Cell<bool>>,
    ax: &AxisState,
    cell: Rc<Cell<CurveData>>,
    all_cells: &HashMap<u16, Rc<Cell<CurveData>>>,
) -> adw::ExpanderRow {
    let title = if ax.desc.is_empty() {
        escape(&ax.name)
    } else {
        format!("{} — {}", escape(&ax.name), escape(&tr(ax.desc)))
    };
    let exp = adw::ExpanderRow::builder().title(title).build();

    // Aperçu (créé tôt : les contrôles le redessineront).
    let area = gtk4::DrawingArea::new();
    area.set_content_height(150);
    area.set_hexpand(true);
    {
        let cell = Rc::clone(&cell);
        area.set_draw_func(move |a, cr, w, h| draw_axis_curve(a, cr, w, h, cell.get()));
    }

    // Sous-titre reflétant l'état (inversion/courbe résumés).
    update_expander_subtitle(&exp, cell.get());

    let cur = cell.get();

    // Fabrique un "on change" qui persiste + ré-applique + redessine.
    macro_rules! commit {
        ($body:expr) => {{
            let inner_rc = Rc::clone(inner_rc);
            let guard = Rc::clone(guard);
            let cell = Rc::clone(&cell);
            let area = area.clone();
            let exp = exp.clone();
            move |val| {
                let mut c = cell.get();
                #[allow(clippy::redundant_closure_call)]
                ($body)(&mut c, val);
                cell.set(c);
                area.queue_draw();
                update_expander_subtitle(&exp, c);
                on_change(&inner_rc, &guard);
            }
        }};
    }

    // Inversion.
    let inv = adw::SwitchRow::builder()
        .title(tr("Invert direction"))
        .active(cur.is_reversed)
        .build();
    {
        let f = commit!(|c: &mut CurveData, active: bool| c.is_reversed = active);
        inv.connect_active_notify(move |r| f(r.is_active()));
    }
    exp.add_row(&inv);

    // Forme S/J.
    let s_label = tr("S (sigmoid)");
    let j_label = tr("J (exponential)");
    let model = gtk4::StringList::new(&[s_label.as_str(), j_label.as_str()]);
    let combo = adw::ComboRow::builder().title(tr("Curve shape")).model(&model).build();
    combo.set_selected(if cur.curve_type == CurveType::J { 1 } else { 0 });
    {
        let f = commit!(|c: &mut CurveData, sel: u32| {
            c.curve_type = if sel == 1 { CurveType::J } else { CurveType::S };
        });
        combo.connect_selected_notify(move |r| f(r.selected()));
    }
    exp.add_row(&combo);

    // Courbure et gain.
    exp.add_row(&spin_row(
        &tr("Curvature"),
        &tr("Negative: more sensitive at center. Positive: softer."),
        -18.0,
        18.0,
        f64::from(cur.curve),
        commit!(|c: &mut CurveData, v: f64| c.curve = v as i8),
    ));
    exp.add_row(&spin_row(
        &tr("Gain"),
        &tr("Amplifies (positive) or attenuates (negative) the output."),
        -10.0,
        10.0,
        f64::from(cur.zoom),
        commit!(|c: &mut CurveData, v: f64| c.zoom = v as i8),
    ));

    // Deadzones / bornes (en % de course).
    exp.add_row(&spin_row(
        &tr("Dead travel (min)"),
        &tr("% of travel ignored at the minimum."),
        0.0,
        50.0,
        f64::from(cur.lower),
        commit!(|c: &mut CurveData, v: f64| c.lower = v as u8),
    ));
    exp.add_row(&spin_row(
        &tr("Saturation (max)"),
        &tr("% before reaching the maximum."),
        0.0,
        50.0,
        f64::from(cur.upper),
        commit!(|c: &mut CurveData, v: f64| c.upper = v as u8),
    ));
    if ax.centered {
        exp.add_row(&spin_row(
            &tr("Center deadzone (low)"),
            &tr("% of deadzone below the center."),
            0.0,
            50.0,
            f64::from(cur.center_lower),
            commit!(|c: &mut CurveData, v: f64| c.center_lower = v as u8),
        ));
        exp.add_row(&spin_row(
            &tr("Center deadzone (high)"),
            &tr("% of deadzone above the center."),
            0.0,
            50.0,
            f64::from(cur.center_upper),
            commit!(|c: &mut CurveData, v: f64| c.center_upper = v as u8),
        ));
    }

    // Point de contrôle (Bézier) — déforme la courbe vers (X, Y). Réglable aux
    // champs OU en **déplaçant la poignée** sur l'aperçu (drag, voir plus bas).
    let xrow = spin_row(
        &tr("Control point X"),
        &tr("Horizontal position of the point (50 = neutral) — or drag the handle on the preview."),
        1.0,
        99.0,
        f64::from(cur.x_pos),
        commit!(|c: &mut CurveData, v: f64| c.x_pos = v as u8),
    );
    let yrow = spin_row(
        &tr("Control point Y"),
        &tr("Vertical position of the point (50 = neutral)."),
        1.0,
        99.0,
        f64::from(cur.y_pos),
        commit!(|c: &mut CurveData, v: f64| c.y_pos = v as u8),
    );
    exp.add_row(&xrow);
    exp.add_row(&yrow);

    // Glisser-déposer de la poignée sur l'aperçu : met à jour le point de contrôle
    // en direct ; au relâcher, recopie sur les champs (persiste + réapplique).
    {
        let drag = gtk4::GestureDrag::new();
        drag.set_button(gtk4::gdk::BUTTON_PRIMARY);
        let start = Rc::new(Cell::new((0.0f64, 0.0f64)));
        {
            let (area, cell, start) = (area.clone(), Rc::clone(&cell), Rc::clone(&start));
            drag.connect_drag_begin(move |_, x, y| {
                start.set((x, y));
                set_control_from_pointer(&area, &cell, x, y);
                area.queue_draw();
            });
        }
        {
            let (area, cell, start) = (area.clone(), Rc::clone(&cell), Rc::clone(&start));
            drag.connect_drag_update(move |_, ox, oy| {
                let (sx, sy) = start.get();
                set_control_from_pointer(&area, &cell, sx + ox, sy + oy);
                area.queue_draw();
            });
        }
        {
            let (xrow, yrow, cell) = (xrow.clone(), yrow.clone(), Rc::clone(&cell));
            drag.connect_drag_end(move |_, _, _| {
                let c = cell.get();
                // Recopie sur les champs → déclenche la persistance + réapplication.
                xrow.set_value(f64::from(c.x_pos));
                yrow.set_value(f64::from(c.y_pos));
            });
        }
        area.add_controller(drag);
    }

    // Rotation de couple : seulement sur l'axe PRIMAIRE d'un couple présent.
    if let Some(&(_a, partner, group)) = ROT_PAIRS
        .iter()
        .find(|(a, b, _)| *a == ax.code && all_cells.contains_key(b))
    {
        let partner_cell = all_cells.get(&partner).cloned();
        let plane = if ax.code == ABS_X { "X/Y" } else { "mini-stick" };
        let sub = tr_f(
            "Rotates the {} plane from −25° to 25° (compensates for a crooked mount). \
             Not visible in the 1D preview.",
            &[plane],
        );
        let adj = gtk4::Adjustment::new(f64::from(cur.rotate), -25.0, 25.0, 1.0, 1.0, 0.0);
        let rot = adw::SpinRow::builder()
            .title(tr("Pair rotation (°)"))
            .subtitle(sub)
            .adjustment(&adj)
            .build();
        let inner_rc2 = Rc::clone(inner_rc);
        let guard2 = Rc::clone(guard);
        let cell2 = Rc::clone(&cell);
        let exp2 = exp.clone();
        rot.connect_value_notify(move |r| {
            let ang = r.value() as i8;
            let grp = if ang != 0 { group } else { 0 };
            let apply_rot = |cell: &Rc<Cell<CurveData>>| {
                let mut c = cell.get();
                c.rotate = ang;
                c.rotate_group = grp;
                cell.set(c);
            };
            apply_rot(&cell2);
            if let Some(p) = &partner_cell {
                apply_rot(p);
            }
            update_expander_subtitle(&exp2, cell2.get());
            on_change(&inner_rc2, &guard2);
        });
        exp.add_row(&rot);
    }

    // Aperçu dans une ligne dédiée.
    let preview_row = gtk4::ListBoxRow::new();
    preview_row.set_activatable(false);
    preview_row.set_selectable(false);
    let frame = gtk4::Frame::new(None);
    frame.set_margin_top(6);
    frame.set_margin_bottom(6);
    frame.set_margin_start(6);
    frame.set_margin_end(6);
    frame.set_child(Some(&area));
    preview_row.set_child(Some(&frame));
    exp.add_row(&preview_row);

    // Réinitialiser cet axe.
    let reset_row = adw::ActionRow::builder().title(tr("Reset this axis")).build();
    let reset_btn = gtk4::Button::builder()
        .icon_name("edit-clear-symbolic")
        .valign(gtk4::Align::Center)
        .build();
    reset_btn.add_css_class("flat");
    reset_row.add_suffix(&reset_btn);
    reset_row.set_activatable_widget(Some(&reset_btn));
    {
        let inner_rc = Rc::clone(inner_rc);
        let guard = Rc::clone(guard);
        let cell = Rc::clone(&cell);
        // Partenaire de rotation à réinitialiser aussi (si axe primaire d'un couple).
        let partner_cell = ROT_PAIRS
            .iter()
            .find(|(a, _, _)| *a == ax.code)
            .and_then(|(_, b, _)| all_cells.get(b).cloned());
        reset_btn.connect_clicked(move |_| {
            cell.set(CurveData::default());
            if let Some(p) = &partner_cell {
                let mut pc = p.get();
                pc.rotate = 0;
                pc.rotate_group = 0;
                p.set(pc);
            }
            on_change(&inner_rc, &guard);
            // Reconstruit la page pour rafraîchir tous les contrôles à leur défaut.
            let dev = inner_rc.borrow().dev.clone();
            if let Some(dev) = dev {
                build_content(&inner_rc, &guard, &dev);
            }
        });
    }
    exp.add_row(&reset_row);

    // Vérif headless uniquement : déplie l'axe nommé par WINWING_EXPAND.
    #[cfg(feature = "screenshot")]
    if std::env::var("WINWING_EXPAND").ok().as_deref() == Some(ax.name.as_str()) {
        exp.set_expanded(true);
    }

    exp
}

/// SpinRow générique bornée, avec un callback `on_change(f64)`.
fn spin_row<F: Fn(f64) + 'static>(
    title: &str,
    subtitle: &str,
    min: f64,
    max: f64,
    value: f64,
    on_change: F,
) -> adw::SpinRow {
    let adj = gtk4::Adjustment::new(value, min, max, 1.0, 1.0, 0.0);
    let row = adw::SpinRow::builder()
        .title(title)
        .subtitle(subtitle)
        .adjustment(&adj)
        .build();
    row.connect_value_notify(move |r| on_change(r.value()));
    row
}

/// Résume l'état d'un axe dans le sous-titre de son ExpanderRow.
fn update_expander_subtitle(exp: &adw::ExpanderRow, c: CurveData) {
    if c.is_identity() && !c.has_rotation() {
        exp.set_subtitle(&tr("Linear (no setting)"));
        return;
    }
    let mut parts: Vec<String> = Vec::new();
    if c.is_reversed {
        parts.push(tr("inverted"));
    }
    if c.curve != 0 {
        parts.push(tr_f(
            "curve {} {}",
            &[c.curve_type.as_str(), format!("{:+}", c.curve).as_str()],
        ));
    }
    if c.zoom != 0 {
        parts.push(tr_f("gain {}", &[format!("{:+}", c.zoom).as_str()]));
    }
    if c.lower != 0 || c.upper != 0 {
        parts.push(tr_f("bounds {}/{}", &[c.lower.to_string().as_str(), c.upper.to_string().as_str()]));
    }
    if c.center_lower != 0 || c.center_upper != 0 {
        parts.push(tr_f("center {}/{}", &[c.center_lower.to_string().as_str(), c.center_upper.to_string().as_str()]));
    }
    if c.x_pos != 50 || c.y_pos != 50 {
        parts.push(tr_f("point {}/{}", &[c.x_pos.to_string().as_str(), c.y_pos.to_string().as_str()]));
    }
    if c.rotate != 0 {
        parts.push(tr_f("rotation {}°", &[c.rotate.to_string().as_str()]));
    }
    if parts.is_empty() {
        // Cas d'un axe où seul `rotate_group` reste (ne devrait pas arriver).
        exp.set_subtitle(&tr("Linear (no setting)"));
    } else {
        exp.set_subtitle(&parts.join(" · "));
    }
}

/// Dessine la courbe de réponse : entrée (X, gauche→droite) → sortie (Y, bas→haut).
fn draw_axis_curve(area: &gtk4::DrawingArea, cr: &gtk4::cairo::Context, w: i32, h: i32, c: CurveData) {
    let (w, h) = (w as f64, h as f64);
    let fg = area.color();
    let (fr, fgc, fb) = (fg.red() as f64, fg.green() as f64, fg.blue() as f64);
    let (ar, ag, ab) = ACCENT;
    let (lpad, rpad, tpad, bpad) = (10.0, 10.0, 10.0, 10.0);
    let (pl, pr, pt, pb) = (lpad, w - rpad, tpad, h - bpad);

    // Grille + diagonale d'identité (repère).
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
    cr.set_source_rgba(fr, fgc, fb, 0.18);
    cr.move_to(pl, pb);
    cr.line_to(pr, pt);
    let _ = cr.stroke();

    // Courbe échantillonnée sur un axe normalisé [0, 1000].
    const N: i32 = 1000;
    cr.set_source_rgb(ar, ag, ab);
    cr.set_line_width(2.5);
    for i in 0..=128 {
        let raw = N * i / 128;
        let out = c.apply(raw, 0, N) as f64 / N as f64;
        let x = pl + (pr - pl) * (i as f64 / 128.0);
        let y = pb - (pb - pt) * out;
        if i == 0 {
            cr.move_to(x, y);
        } else {
            cr.line_to(x, y);
        }
    }
    let _ = cr.stroke();

    // Poignée du point de contrôle (déplaçable). Position = (x_pos, y_pos) en %.
    let hx = pl + (pr - pl) * f64::from(c.x_pos) / 100.0;
    let hy = pb - (pb - pt) * f64::from(c.y_pos) / 100.0;
    cr.set_source_rgb(ar, ag, ab);
    cr.arc(hx, hy, 6.0, 0.0, std::f64::consts::TAU);
    let _ = cr.fill();
    // Anneau blanc pour la lisibilité sur la courbe.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.set_line_width(1.5);
    cr.arc(hx, hy, 6.0, 0.0, std::f64::consts::TAU);
    let _ = cr.stroke();
}

/// Convertit une position pointeur (coords widget) en point de contrôle
/// `x_pos`/`y_pos` (1..=99) et le pose dans la cellule. Même repère que le tracé.
fn set_control_from_pointer(area: &gtk4::DrawingArea, cell: &Rc<Cell<CurveData>>, px: f64, py: f64) {
    let (w, h) = (area.width() as f64, area.height() as f64);
    let (pl, pr, pt, pb) = (10.0, w - 10.0, 10.0, h - 10.0);
    if pr <= pl || pb <= pt {
        return;
    }
    let x = ((px - pl) / (pr - pl)).clamp(0.0, 1.0);
    let y = ((pb - py) / (pb - pt)).clamp(0.0, 1.0);
    let mut c = cell.get();
    c.x_pos = (x * 100.0).round().clamp(1.0, 99.0) as u8;
    c.y_pos = (y * 100.0).round().clamp(1.0, 99.0) as u8;
    cell.set(c);
}

// --- Persistance + application --------------------------------------------
/// Collecte les cellules -> `AxisCurves` -> disque. Si un démon tourne, le
/// redémarre pour appliquer les nouvelles courbes immédiatement.
fn on_change(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    let (dev, curves, active) = {
        let inner = inner_rc.borrow();
        let Some(dev) = inner.dev.clone() else { return };
        let mut curves = axis_store::AxisCurves::new();
        for cell in &inner.cells {
            curves.set(cell.code, cell.curve.get());
        }
        (dev, curves, inner.session.is_some())
    };
    if let Err(e) = axis_store::save_curves(&dev, &curves, None) {
        toast(inner_rc, &format!("Enregistrement impossible : {e}"));
    }
    if active {
        // Ré-applique : redémarre le démon avec le plan à jour.
        start_daemon(inner_rc, guard, &dev);
    }
}

// --- Démon « courbe seule » (mode "curve") --------------------------------
fn start_daemon(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>, dev: &WinwingDevice) {
    if dev.evdev.is_empty() {
        toast(inner_rc, &tr("Input device not found for this joystick"));
        refresh_switch(inner_rc, guard);
        return;
    }
    let curves = {
        let inner = inner_rc.borrow();
        let mut curves = HashMap::new();
        for cell in &inner.cells {
            let c = cell.curve.get();
            // Inclure aussi les axes en rotation seule (identité côté `apply`).
            if !c.is_identity() || c.has_rotation() {
                curves.insert(cell.code, c);
            }
        }
        curves
    };
    if curves.is_empty() {
        toast(
            inner_rc,
            &tr("No axis inverted, curved or rotated — set an axis first"),
        );
        refresh_switch(inner_rc, guard);
        return;
    }
    {
        let mut inner = inner_rc.borrow_mut();
        teardown_session(&mut inner, true);
    }
    let li = match LiveInput::open(&dev.evdev) {
        Ok(l) => l,
        Err(e) => {
            toast(inner_rc, &tr_f("Cannot open the input device: {}", &[e.to_string().as_str()]));
            refresh_switch(inner_rc, guard);
            return;
        }
    };
    let plan = match remap::build_plan(&li, "curve", &std::collections::HashMap::new(), &curves) {
        Ok(p) => p,
        Err(e) => {
            toast(inner_rc, &tr_f("Invalid plan: {}", &[e.to_string().as_str()]));
            refresh_switch(inner_rc, guard);
            return;
        }
    };
    let mut sess = remap::RemapSession::new(li, plan);
    if let Err(e) = sess.start() {
        toast(
            inner_rc,
            &tr_f(
                "Virtual device unavailable: {}. Is a split or remap already active (Remap tab)? \
                 Otherwise, check that the \"uinput\" module is loaded and that the access rule \
                 is installed.",
                &[e.to_string().as_str()],
            ),
        );
        refresh_switch(inner_rc, guard);
        return;
    }
    {
        let mut inner = inner_rc.borrow_mut();
        inner.session = Some(sess);
        let ir = Rc::clone(inner_rc);
        let g = Rc::clone(guard);
        let src = glib::source::timeout_add_local(PUMP_INTERVAL, move || on_pump(&ir, &g));
        inner.session_source = Some(src);
    }
    toast(inner_rc, &tr("Inversion/curves active — virtual device"));
    refresh_switch(inner_rc, guard);
}

fn stop_daemon(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>, message: &str) {
    {
        let mut inner = inner_rc.borrow_mut();
        teardown_session(&mut inner, true);
    }
    refresh_switch(inner_rc, guard);
    toast(inner_rc, message);
}

fn on_pump(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) -> glib::ControlFlow {
    let gone = {
        let mut inner = inner_rc.borrow_mut();
        match inner.session.as_mut() {
            Some(s) => s.pump().is_err(),
            None => return glib::ControlFlow::Break,
        }
    };
    if gone {
        {
            let mut inner = inner_rc.borrow_mut();
            teardown_session(&mut inner, false); // on EST la source
        }
        refresh_switch(inner_rc, guard);
        toast(inner_rc, &tr("Application stopped — joystick disconnected"));
        return glib::ControlFlow::Break;
    }
    glib::ControlFlow::Continue
}

fn teardown_session(inner: &mut Inner, remove_source: bool) {
    if remove_source {
        if let Some(s) = inner.session_source.take() {
            s.remove();
        }
    } else {
        inner.session_source = None;
    }
    inner.session = None;
}

/// Reflète l'état de la session sur l'interrupteur d'activation (sans réentrance).
fn refresh_switch(inner_rc: &Rc<RefCell<Inner>>, guard: &Rc<Cell<bool>>) {
    let (sw, active) = {
        let inner = inner_rc.borrow();
        (inner.activate_switch.clone(), inner.session.is_some())
    };
    if let Some(sw) = sw {
        guard.set(true);
        sw.set_active(active);
        guard.set(false);
    }
}

fn toast(inner_rc: &Rc<RefCell<Inner>>, text: &str) {
    let overlay = inner_rc.borrow().overlay.clone();
    overlay.add_toast(adw::Toast::new(text));
}

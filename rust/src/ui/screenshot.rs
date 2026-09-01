//! Capture offscreen de la fenêtre — outil de VÉRIFICATION headless, derrière la
//! feature `screenshot` (jamais dans un build normal). Si `WINWING_SHOT` désigne
//! un fichier, rend la fenêtre en BGRA brut (WidgetPaintable → nœud GSK → Cairo)
//! après un court délai, puis quitte. `convert`/`magick` transforme le BGRA en
//! PNG. Aucune dépendance ajoutée (cairo/gsk viennent de gtk4).

use std::time::Duration;

use gtk4::prelude::*;
use libadwaita as adw;

/// Programme la capture puis la fermeture si `WINWING_SHOT` est défini.
pub fn arm(window: &adw::ApplicationWindow, app: &adw::Application) {
    let Ok(path) = std::env::var("WINWING_SHOT") else {
        return;
    };
    let window = window.clone();
    let app = app.clone();
    gtk4::glib::timeout_add_local_once(Duration::from_millis(1800), move || {
        dump(&window, &path);
        app.quit();
    });
}

fn dump(window: &adw::ApplicationWindow, path: &str) {
    let w = window.width().max(1);
    let h = window.height().max(1);
    let paintable = gtk4::WidgetPaintable::new(Some(window));
    let snapshot = gtk4::Snapshot::new();
    paintable.snapshot(&snapshot, w as f64, h as f64);
    let Some(node) = snapshot.to_node() else {
        eprintln!("[shot] snapshot vide");
        return;
    };
    let mut surface =
        gtk4::cairo::ImageSurface::create(gtk4::cairo::Format::ARgb32, w, h).unwrap();
    {
        let cr = gtk4::cairo::Context::new(&surface).unwrap();
        node.draw(&cr);
    }
    surface.flush();
    let stride = surface.stride() as usize;
    let rowbytes = w as usize * 4;
    let data = surface.data().unwrap();
    let mut out = Vec::with_capacity(rowbytes * h as usize);
    for y in 0..h as usize {
        out.extend_from_slice(&data[y * stride..y * stride + rowbytes]);
    }
    std::fs::write(path, &out).unwrap();
    eprintln!("[shot] {w}x{h} -> {path}");
}

//! Provider CSS unique de l'app + suivi du thème système (clair/sombre).
//!
//! On reste THÈME-AWARE (suit GNOME) : les couleurs custom sont dérivées des
//! couleurs NOMMÉES libadwaita (@accent_color, @success_color, @window_fg_color,
//! @card_bg_color…) via `alpha()`, donc elles s'adaptent seules au clair comme
//! au sombre. Ce provider regroupe aussi le style du moniteur d'entrées en direct.

const CSS: &str = "\
/* --- Moniteur live (consolidé depuis la page Boutons) --- */\n\
.winwing-btn { min-width: 20px; padding: 2px 7px; border-radius: 7px; background: alpha(currentColor, 0.10); }\n\
.winwing-btn.pressed { background: @accent_bg_color; color: @accent_fg_color; font-weight: bold; }\n\
.winwing-axisbar { min-height: 10px; }\n\
\n\
/* --- Pastilles (adaptatives via couleurs nommées) --- */\n\
.wl-pill { border-radius: 20px; padding: 3px 11px; font-size: 11px; font-weight: bold; }\n\
.wl-pill.accent  { background: alpha(@accent_color, 0.15);      color: @accent_color; }\n\
.wl-pill.success { background: alpha(@success_color, 0.15);     color: @success_color; }\n\
.wl-pill.neutral { background: alpha(@window_fg_color, 0.08);   color: @window_fg_color; }\n\
\n\
/* --- Logo carré accent (chrome) --- */\n\
.wl-logo { background: @accent_bg_color; border-radius: 6px; min-width: 22px; min-height: 22px; }\n\
.wl-logo image { color: @accent_fg_color; }\n\
\n\
/* --- Tuile photo sombre (showcase, sombre dans les deux thèmes) --- */\n\
.wl-phototile { background: #101216; border: 1px solid #23262d; border-radius: 14px; padding: 14px; }\n\
.wl-phototile-caption { color: #8a93a3; font-size: 11px; }\n\
\n\
/* --- Carte d'aperçu générique (courbe, bandeau) --- */\n\
.wl-card { background: @card_bg_color; border: 1px solid alpha(@window_fg_color, 0.10); border-radius: 14px; }\n\
\n\
/* --- Tuile dégradée derrière la photo du manche (bandeau) --- */\n\
.wl-stick-tile { background: linear-gradient(160deg, alpha(@window_fg_color, 0.04), alpha(@window_fg_color, 0.10)); border-radius: 12px; }\n\
\n\
/* --- Segment actif d'un choix (mode d'éclairage) --- */\n\
.wl-seg-active { background: @accent_bg_color; color: @accent_fg_color; font-weight: bold; }\n\
\n\
/* --- Point d'état (pied de barre latérale) --- */\n\
.wl-dot { min-width: 7px; min-height: 7px; border-radius: 50%; background: @success_color; }\n\
";

/// Installe le provider CSS global sur l'affichage par défaut.
/// (Le thème clair/sombre est géré par libadwaita via les couleurs nommées ;
/// aucune classe racine à basculer — les `alpha(@…)` s'adaptent seules.)
pub fn install() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_data(CSS);
    if let Some(display) = gtk4::gdk::Display::default() {
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

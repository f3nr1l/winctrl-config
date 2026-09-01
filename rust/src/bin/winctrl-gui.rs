//! `winctrl-gui` — binaire de l'application de configuration (GTK4/libadwaita).
//!
//! L'I/O device tourne hors du thread UI (cf. `winctrl_config::ui`). Toute
//! écriture matérielle passe par une confirmation explicite avec sauvegarde.

fn main() -> winctrl_config::ui::ExitCode {
    winctrl_config::ui::run()
}

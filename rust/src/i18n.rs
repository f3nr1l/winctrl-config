//! Internationalisation (gettext).
//!
//! Les chaînes source de l'interface et de la CLI sont en **anglais** ; les
//! catalogues de traduction vivent dans `po/` et sont installés en
//! `<localedir>/<langue>/LC_MESSAGES/winctrl.mo`. L'anglais est donc le repli
//! naturel quand aucun catalogue n'est trouvé.
//!
//! Toutes les chaînes visibles passent par [`tr`] / [`tr_f`] / [`trn`] / [`trn_f`].
//! Sans la feature `gui` (cœur/CLI purs), ces fonctions renvoient la chaîne
//! source telle quelle (aucune dépendance gettext tirée).

/// Nom de domaine gettext (= nom de base des catalogues `.mo`).
pub const DOMAIN: &str = "winctrl";

#[cfg(feature = "gui")]
mod imp {
    use super::DOMAIN;
    use gettextrs::{
        bind_textdomain_codeset, bindtextdomain, setlocale, textdomain, LocaleCategory,
    };
    use std::path::{Path, PathBuf};

    /// Répertoire de base des catalogues. `WINCTRL_LOCALEDIR` l'emporte ; sinon on
    /// retient la première base candidate qui contient réellement le domaine, avec
    /// `/usr/share/locale` en dernier recours.
    fn locale_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("WINCTRL_LOCALEDIR") {
            return PathBuf::from(d);
        }
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Some(home) = std::env::var_os("HOME") {
            candidates.push(PathBuf::from(home).join(".local/share/locale"));
        }
        candidates.push(PathBuf::from("/app/share/locale")); // Flatpak
        candidates.push(PathBuf::from("/usr/local/share/locale"));
        candidates.push(PathBuf::from("/usr/share/locale"));
        candidates
            .iter()
            .find(|base| dir_has_domain(base))
            .cloned()
            .unwrap_or_else(|| PathBuf::from("/usr/share/locale"))
    }

    /// `true` si `<base>/<langue>/LC_MESSAGES/<domain>.mo` existe pour au moins
    /// une langue.
    fn dir_has_domain(base: &Path) -> bool {
        let Ok(rd) = std::fs::read_dir(base) else {
            return false;
        };
        rd.flatten().any(|e| {
            e.path()
                .join("LC_MESSAGES")
                .join(format!("{DOMAIN}.mo"))
                .is_file()
        })
    }

    pub fn init() {
        let _ = setlocale(LocaleCategory::LcAll, "");
        let _ = bindtextdomain(DOMAIN, locale_dir());
        let _ = bind_textdomain_codeset(DOMAIN, "UTF-8");
        let _ = textdomain(DOMAIN);
    }

    pub fn tr(msgid: &str) -> String {
        gettextrs::gettext(msgid)
    }

    pub fn trn(singular: &str, plural: &str, n: u32) -> String {
        gettextrs::ngettext(singular, plural, n)
    }
}

#[cfg(not(feature = "gui"))]
mod imp {
    pub fn init() {}
    pub fn tr(msgid: &str) -> String {
        msgid.to_string()
    }
    pub fn trn(singular: &str, plural: &str, n: u32) -> String {
        if n == 1 {
            singular.to_string()
        } else {
            plural.to_string()
        }
    }
}

pub use imp::init;

/// Substitue les marqueurs `{}` de `template` par `args`, dans l'ordre.
fn interpolate(mut template: String, args: &[&str]) -> String {
    for a in args {
        if let Some(pos) = template.find("{}") {
            template.replace_range(pos..pos + 2, a);
        }
    }
    template
}

/// Traduit une chaîne (le `msgid` est le texte anglais source).
pub fn tr(msgid: &str) -> String {
    imp::tr(msgid)
}

/// Traduit `msgid` puis substitue ses `{}` par `args`, dans l'ordre.
pub fn tr_f(msgid: &str, args: &[&str]) -> String {
    interpolate(imp::tr(msgid), args)
}

/// Forme singulier / pluriel selon `n` (règles de pluriel du catalogue).
pub fn trn(singular: &str, plural: &str, n: u32) -> String {
    imp::trn(singular, plural, n)
}

/// `trn` suivi de la substitution des `{}` (souvent `{}` = `n`).
pub fn trn_f(singular: &str, plural: &str, n: u32, args: &[&str]) -> String {
    interpolate(imp::trn(singular, plural, n), args)
}

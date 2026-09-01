//! `wwctrl` — smoke CLI **lecture seule** : valide transport + modèle sans écran.
//!
//! `wwctrl --list`            liste les périphériques WinWing (parsing /sys).
//! `wwctrl --read /dev/hidrawN`  lit et affiche la config décodée d'un endpoint.
//!
//! Aucune écriture device : `--read` n'émet que des `READ_CFG_DATA` (non
//! destructif). Les writes applicatifs (gate humain) sont hors périmètre.

use std::process::ExitCode;

use winctrl_config::enumerate;
use winctrl_config::i18n::{self, tr, tr_f};
use winctrl_config::model::{self, Transport};
use winctrl_config::protocol as p;
use winctrl_config::transport::HidrawTransport;

fn main() -> ExitCode {
    i18n::init();
    model::migrate_legacy_data_dir();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None | Some("--list") => {
            list();
            ExitCode::SUCCESS
        }
        Some("--read") => match args.get(2) {
            Some(path) => read(path),
            None => {
                eprintln!("{}", tr("usage: wwctrl --read /dev/hidrawN"));
                ExitCode::FAILURE
            }
        },
        Some("--twist-dryrun") => match args.get(2) {
            Some(path) => twist_dryrun(path),
            None => {
                eprintln!("{}", tr("usage: wwctrl --twist-dryrun /dev/hidrawN"));
                ExitCode::FAILURE
            }
        },
        Some("--backlight-dryrun") => match args.get(2) {
            Some(path) => backlight_dryrun(path),
            None => {
                eprintln!("{}", tr("usage: wwctrl --backlight-dryrun /dev/hidrawN"));
                ExitCode::FAILURE
            }
        },
        Some(other) => {
            eprintln!("{}", tr_f("unknown argument: {}", &[other]));
            eprintln!(
                "{}",
                tr("usage: wwctrl [--list | --read | --twist-dryrun | --backlight-dryrun /dev/hidrawN]")
            );
            ExitCode::FAILURE
        }
    }
}

/// DRY-RUN des writes rétroéclairage (base) : live 0x49, persist 0xEC, mode 0xF8.
/// Imprime les trames SANS rien émettre.
fn backlight_dryrun(path: &str) -> ExitCode {
    let devices = enumerate::discover();
    let Some(dev) = devices.iter().find(|d| d.hidraw == path) else {
        eprintln!("{}", tr_f("no WinWing device at {}", &[path]));
        return ExitCode::FAILURE;
    };
    let Some(c) = dev.controllers.iter().find(|c| c.family == p::FAMILY_BASE) else {
        eprintln!("{}", tr_f("no base (family 0xbb) at {}", &[path]));
        return ExitCode::FAILURE;
    };
    let mut t = match HidrawTransport::open(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}", tr_f("opening {}: {}", &[path, e.to_string().as_str()]));
            return ExitCode::FAILURE;
        }
    };
    println!(
        "DRY-RUN backlight — {} (dev {:#04x} / fam {:#04x}) — NO write emitted",
        c.model(),
        c.device,
        c.family
    );
    let v = 128u8;
    let live = p::build_frame(c.device, c.family, p::OP_SET_LEDX, &[p::LED_INDEX_BACKLIGHT, v]);
    println!("  live 0x49 (val {v})   : FRAME = {}", p::hx(&live));
    if let Ok(out) = model::set_backlight_persist(&mut t, c.device, c.family, v, true, None) {
        println!(
            "  persist 0xEC (val {v}) : new=[{}]  FRAME = {}",
            p::hx(&out.new),
            p::hx(&out.frame)
        );
    }
    for (name, on) in [("breathing", true), ("static", false)] {
        if let Ok(out) = model::set_breathing(&mut t, c.device, c.family, on, true, None) {
            println!(
                "  mode {name:<11} 0xF8  : new=[{}]  TRAME = {}",
                p::hx(&out.new),
                p::hx(&out.frame)
            );
        }
    }
    ExitCode::SUCCESS
}

/// DRY-RUN du write twist (0xD8) : imprime, pour chaque mode, la trame qui
/// SERAIT émise (sans rien écrire). Sert de preuve vs la trame connue-bonne.
fn twist_dryrun(path: &str) -> ExitCode {
    let devices = enumerate::discover();
    let Some(dev) = devices.iter().find(|d| d.hidraw == path) else {
        eprintln!("{}", tr_f("no WinWing device at {}", &[path]));
        return ExitCode::FAILURE;
    };
    let Some(c) = dev.controllers.iter().find(|c| c.family == p::FAMILY_GRIP) else {
        eprintln!("{}", tr_f("no grip (family 0xbf) at {}", &[path]));
        return ExitCode::FAILURE;
    };
    let mut t = match HidrawTransport::open(path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("{}", tr_f("opening {}: {}", &[path, e.to_string().as_str()]));
            return ExitCode::FAILURE;
        }
    };
    println!(
        "DRY-RUN twist 0xD8 — {} (dev {:#04x} / fam {:#04x}) — NO write emitted",
        c.model(),
        c.device,
        c.family
    );
    for (name, mode) in model::TWIST_MODES {
        match model::set_twist_mode(&mut t, c.device, c.family, *mode, true, None) {
            Ok(out) => {
                let old = out.old.map(|b| p::hx(&b)).unwrap_or_else(|| "----".into());
                println!(
                    "  {name:<13} (mode {mode:#04x}) : old=[{old}]  new=[{}]  FRAME = {}",
                    p::hx(&out.new),
                    p::hx(&out.frame)
                );
            }
            Err(e) => println!("  {name:<13} : error — {e}"),
        }
    }
    ExitCode::SUCCESS
}

fn list() {
    let devices = enumerate::discover();
    println!("{}", enumerate::format_list(&devices));
}

fn read(path: &str) -> ExitCode {
    // Retrouve les contrôleurs de cet endpoint via /sys (aucune I/O device).
    let devices = enumerate::discover();
    let Some(dev) = devices.iter().find(|d| d.hidraw == path) else {
        eprintln!("{}", tr_f("no WinWing device at {}", &[path]));
        eprintln!("{}", tr("(try `wwctrl --list`)"));
        return ExitCode::FAILURE;
    };
    if !dev.readable() {
        eprintln!(
            "{}",
            tr_f("warning: {} not accessible for read/write (install the access rule)", &[path])
        );
    }
    match model::read_device::<HidrawTransport>(path, &dev.controllers) {
        Ok(cfg) => {
            print_device(&cfg);
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{}", tr_f("failed to open {}: {}", &[path, e.to_string().as_str()]));
            ExitCode::FAILURE
        }
    }
}

fn print_device(cfg: &model::DeviceConfig) {
    println!("Endpoint {}", cfg.hidraw);
    for c in &cfg.controllers {
        println!(
            "\n== {} (dev {:#04x} / fam {:#04x}) ==",
            c.model, c.device, c.family
        );
        if !c.product_name.is_empty() {
            println!("  product : {}", c.product_name);
        }
        if !c.serial.is_empty() {
            println!("  serial  : {}", c.serial);
        }
        if c.twist_mode.is_some() {
            println!("  twist   : {}", c.twist_label());
        }
        for f in &c.fields {
            let flag = if f.identity { " [protected identity]" } else { "" };
            println!(
                "  {:#06x}  {:<18}  {}   {}{}",
                f.offset,
                f.name,
                f.hex(),
                f.human,
                flag
            );
        }
    }
}

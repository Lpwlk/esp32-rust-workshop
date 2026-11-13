//! `presets`: look at and check the presets without building a firmware.
//!
//! ```text
//! cargo presets list [uart|eth]            presets of one app (or both)
//! cargo presets show eth node-a            every setting a preset results in
//! cargo presets check                      load every preset of both apps (CI runs it)
//! ```
use std::{path::PathBuf, process::ExitCode};

use workshop_presets::{App, describe, list, load};

fn presets_dir(app: App) -> PathBuf {
    let root = std::env::var_os("PRESETS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../presets"));
    root.join(app.folder())
}

fn usage() -> ExitCode {
    eprintln!("usage: presets list [uart|eth]\n       presets show <uart|eth> <name>\n       presets check");
    ExitCode::from(2)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let apps = |filter: Option<&String>| match filter.map(|a| App::parse(a)) {
        None => Some(vec![App::Uart, App::Ethernet]),
        Some(Some(app)) => Some(vec![app]),
        Some(None) => None,
    };
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["list", ..] => {
            let Some(apps) = apps(args.get(1)) else { return usage() };
            for app in apps {
                let dir = presets_dir(app);
                println!("{} ({})", app.folder(), dir.display());
                for name in list(&dir) {
                    match load(&dir, &name, app) {
                        Ok(p) => {
                            // `a <- b <- c`: the files read, root first.
                            let chain: Vec<String> = p
                                .files
                                .iter()
                                .filter_map(|f| f.file_stem().map(|s| s.to_string_lossy().into_owned()))
                                .collect();
                            println!("  {name:<16} node `{}`  ({})", p.config.name, chain.join(" <- "));
                        }
                        Err(e) => println!("  {name:<16} INVALID: {e}"),
                    }
                }
            }
            ExitCode::SUCCESS
        }
        ["show", app, name] => {
            let Some(app) = App::parse(app) else { return usage() };
            match load(&presets_dir(app), name, app) {
                Ok(p) => {
                    println!(
                        "# preset `{}` of the {} app, from: {}",
                        p.name,
                        app.folder(),
                        p.files.iter().map(|f| f.display().to_string()).collect::<Vec<_>>().join(" <- ")
                    );
                    for (k, v) in describe(&p.config) {
                        println!("{k:<24} = {v}");
                    }
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("{e}");
                    ExitCode::FAILURE
                }
            }
        }
        ["check"] => {
            let mut failed = false;
            for app in [App::Uart, App::Ethernet] {
                let dir = presets_dir(app);
                for name in list(&dir) {
                    match load(&dir, &name, app) {
                        Ok(_) => println!("ok       {}/{name}", app.folder()),
                        Err(e) => {
                            failed = true;
                            println!("INVALID  {}/{name}: {e}", app.folder());
                        }
                    }
                }
            }
            if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
        }
        _ => usage(),
    }
}

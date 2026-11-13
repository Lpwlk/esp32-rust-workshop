//! Presets: named, documented configurations of a node, in TOML.
//!
//! A preset is a file `firmware/<app>/presets/<name>.toml` (`<app>` is `uart` or `ethernet`). It lists
//! the settings it changes; everything else comes from the default of the app
//! ([`NodeConfig::DEFAULT`] or [`NodeConfig::DEFAULT_NET`]). `extends = "other"` starts from
//! another preset of the same folder instead (chains are fine, cycles are refused), which is
//! how `node-a` and `node-b` share a `base`.
//!
//! ```toml
//! extends = "base"
//! name    = "node-A"
//! led1    = "peer"
//!
//! [net]
//! address = "192.168.77.11"
//! peer    = "192.168.77.12"
//! ```
//!
//! Keys are the ones of `set <key> <value>` in the CLI (runtime settings), plus the build-time
//! ones (`name`, `pins.*`, `net.*`); a `[table]` is the part of the key before the dot.
//! Values are the text the CLI accepts, so units work (`"2.5Hz"`, `"blink 200ms"`), numbers
//! and booleans may be written naturally (`brightness = 40`, `verbose = 1`).
//!
//! [`load`] checks everything and returns the config; [`rust_source`] is the Rust expression
//! the firmware includes (`const CONFIG: NodeConfig = include!(...)`), which the compiler
//! validates once more (`NodeConfig::check`).

use std::{
    fmt, fs,
    path::{Path, PathBuf},
};

use workshop_core::config::{Key, NodeConfig, SetError};

/// Which firmware a preset is for: it decides the starting point and the folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum App {
    /// The UART firmware (`firmware/uart/presets`).
    Uart,
    /// The Ethernet firmware (`firmware/ethernet/presets`).
    Ethernet,
}

impl App {
    /// The app's short name on the command line: `uart` or `eth`.
    pub fn folder(self) -> &'static str {
        match self {
            App::Uart => "uart",
            App::Ethernet => "eth",
        }
    }

    /// The app's folder in `firmware/`: its presets are in `firmware/<this>/presets/`.
    pub fn firmware_dir(self) -> &'static str {
        match self {
            App::Uart => "uart",
            App::Ethernet => "ethernet",
        }
    }

    /// Parses `uart`, `eth` or `ethernet`.
    pub fn parse(text: &str) -> Option<App> {
        match text {
            "uart" => Some(App::Uart),
            "eth" | "ethernet" => Some(App::Ethernet),
            _ => None,
        }
    }

    /// The config a preset starts from when it extends nothing.
    pub fn defaults(self) -> NodeConfig {
        match self {
            App::Uart => NodeConfig::DEFAULT,
            App::Ethernet => NodeConfig::DEFAULT_NET,
        }
    }
}

/// A preset, loaded and checked.
#[derive(Clone, Debug)]
pub struct Preset {
    /// Its name (the file name without `.toml`).
    pub name: String,
    /// The files it was built from, the one that was asked for last (use for
    /// `cargo:rerun-if-changed`).
    pub files: Vec<PathBuf>,
    /// The resulting config, valid.
    pub config: NodeConfig,
}

/// Why a preset was refused. `Display` gives the message to show, naming the file and key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

fn err<T>(message: impl Into<String>) -> Result<T, Error> {
    Err(Error(message.into()))
}

/// Names of the presets in `dir`, sorted (`base` included).
pub fn list(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.strip_suffix(".toml").map(str::to_owned))
        .collect();
    names.sort();
    names
}

/// One file of an `extends` chain.
struct Layer {
    path: PathBuf,
    table: toml::Table,
}

/// Reads `name` and what it extends, root first.
fn chain(dir: &Path, name: &str) -> Result<Vec<Layer>, Error> {
    let mut layers: Vec<Layer> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    let mut next = Some(name.to_owned());
    while let Some(current) = next.take() {
        if seen.contains(&current) {
            return err(format!("presets: `{}` extends itself (cycle: {} -> {current})", current, seen.join(" -> ")));
        }
        if !current.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') || current.is_empty() {
            return err(format!("presets: bad preset name `{current}` (letters, digits, - and _ only)"));
        }
        let path = dir.join(format!("{current}.toml"));
        let text = fs::read_to_string(&path).map_err(|e| {
            let available = list(dir).join(", ");
            Error(format!("presets: can't read {} ({e}). Available in {}: {available}", path.display(), dir.display()))
        })?;
        let table: toml::Table =
            text.parse().map_err(|e| Error(format!("presets: {} is not valid TOML:\n{e}", path.display())))?;
        next = match table.get("extends") {
            None => None,
            Some(toml::Value::String(parent)) => Some(parent.clone()),
            Some(_) => return err(format!("presets: {}: `extends` must be a preset name (a string)", path.display())),
        };
        seen.push(current);
        layers.push(Layer { path, table });
    }
    layers.reverse();
    Ok(layers)
}

/// `key = value` pairs of a table, with `[tables]` flattened into `table.key`.
fn flatten(path: &Path, table: &toml::Table) -> Result<Vec<(String, String)>, Error> {
    const TABLES: [&str; 4] = ["pins", "rgb", "burst", "net"];
    let mut out = Vec::new();
    let scalar = |key: &str, value: &toml::Value| -> Result<String, Error> {
        Ok(match value {
            toml::Value::String(s) => s.clone(),
            toml::Value::Integer(i) => i.to_string(),
            toml::Value::Float(f) => f.to_string(),
            toml::Value::Boolean(b) => (if *b { "on" } else { "off" }).to_owned(),
            _ => return err(format!("presets: {}: `{key}` must be a string, a number or a boolean", path.display())),
        })
    };
    for (key, value) in table {
        match value {
            toml::Value::Table(sub) if TABLES.contains(&key.as_str()) => {
                for (k, v) in sub {
                    let full = format!("{key}.{k}");
                    out.push((full.clone(), scalar(&full, v)?));
                }
            }
            toml::Value::Table(_) => {
                return err(format!(
                    "presets: {}: unknown section [{key}] (sections: {})",
                    path.display(),
                    TABLES.join(", ")
                ));
            }
            _ if key == "extends" => {}
            _ => out.push((key.clone(), scalar(key, value)?)),
        }
    }
    Ok(out)
}

/// Applies the settings of one file. Build-time keys first, then `tx_mode`, then the rest, so
/// that an explicit `data_period_ms` wins over the period a `tx_mode = "periodic 2.5Hz"` carries.
fn apply(config: &mut NodeConfig, path: &Path, pairs: Vec<(String, String)>) -> Result<(), Error> {
    let rank = |key: &str| match key {
        "name" => 0,
        k if k.starts_with("pins.") || k.starts_with("net.") => 1,
        "tx_mode" => 2,
        _ => 3,
    };
    let mut pairs = pairs;
    pairs.sort_by_key(|(k, _)| rank(k));
    for (key, value) in pairs {
        let at = |what: String| Error(format!("presets: {}: `{key}`: {what}", path.display()));
        if key == "name" {
            // The name lives in the firmware as a `&'static str`: leak it, this is a build script.
            config.set_name(Box::leak(value.clone().into_boxed_str()));
            continue;
        }
        // A plain setting written after a `[section]` header lands in that section: say so.
        let hint = if key.contains('.') {
            " (every key after a [section] header belongs to it: write plain settings before the first [section])"
        } else {
            ""
        };
        config.set_build(&key, &value).map_err(|e| match e {
            SetError::UnknownKey => at(format!(
                "unknown setting{hint}. Runtime settings: {}; build-time: name, pins.*, net.* (see docs/book/reference/)",
                workshop_core::config::KEYS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            )),
            SetError::ReadOnly => at("read-only".into()),
            SetError::BadValue(expected) => at(format!("expected {expected}, got `{value}`{hint}")),
            SetError::Invalid(msg) => at(msg.trim_start_matches("config: ").to_owned()),
        })?;
    }
    Ok(())
}

/// Loads preset `name` of `app` from `dir`, applies its `extends` chain, and validates the result.
pub fn load(dir: &Path, name: &str, app: App) -> Result<Preset, Error> {
    let layers = chain(dir, name)?;
    let mut config = app.defaults();
    let mut files = Vec::new();
    for layer in layers {
        let pairs = flatten(&layer.path, &layer.table)?;
        apply(&mut config, &layer.path, pairs)?;
        files.push(layer.path);
    }
    config.validate().map_err(|msg| {
        Error(format!(
            "presets: `{name}` ({}): {}",
            files.last().map_or_else(String::new, |p| p.display().to_string()),
            msg.trim_start_matches("config: ")
        ))
    })?;
    Ok(Preset { name: name.to_owned(), files, config })
}

/// The Rust expression of a preset's config, for `include!`.
pub fn rust_source(preset: &Preset) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    let files: Vec<String> =
        preset.files.iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
    let _ = writeln!(
        out,
        "// Generated by workshop-presets from preset `{}` ({}). Do not edit.",
        preset.name,
        files.join(", ")
    );
    out.push_str("{\n    use workshop_core::config::*;\n");
    let mut body = String::new();
    preset.config.write_rust(&mut body).expect("writing to a String");
    for line in body.lines() {
        let _ = writeln!(out, "    {line}");
    }
    out.push_str("}\n");
    out
}

/// What a build script does: loads `name`, writes `<out_dir>/<file>.rs`, and tells Cargo what to
/// watch. Panics with the message of [`Error`] (the build stops, the message is shown).
pub fn generate_for_build(app: App, presets_dir: &Path, name: &str, out_file: &Path) {
    let preset = match load(presets_dir, name, app) {
        Ok(p) => p,
        Err(e) => panic!("\n\n{e}\n\n"),
    };
    fs::write(out_file, rust_source(&preset)).expect("can't write the generated preset");
    for f in &preset.files {
        println!("cargo:rerun-if-changed={}", f.display());
    }
    // A new file in the folder (a preset added, or fixing a missing one) must also trigger it.
    println!("cargo:rerun-if-changed={}", presets_dir.display());
}

/// Every setting of a config as `key = value`, build-time ones first (for `presets show`).
pub fn describe(config: &NodeConfig) -> Vec<(String, String)> {
    use workshop_core::config::Link;
    let mut rows = vec![("name".to_owned(), config.name.to_owned())];
    rows.push(("pins.button".into(), config.pins.button.to_string()));
    rows.push(("pins.led1".into(), config.pins.led1.to_string()));
    rows.push(("pins.led2".into(), config.pins.led2.to_string()));
    match config.link {
        Link::Uart(u) => {
            rows.push(("pins.uart_tx".into(), u.tx.to_string()));
            rows.push(("pins.uart_rx".into(), u.rx.to_string()));
        }
        Link::Net(n) => {
            for (key, _, _) in workshop_core::net::NET_KEYS {
                let mut v = String::new();
                n.write_value(key, &mut v).expect("String");
                rows.push((key.to_owned(), v));
            }
        }
    }
    for (name, key) in workshop_core::config::KEYS {
        rows.push((name.to_owned(), config.value(Key::from_name(name).unwrap_or(key)).to_string()));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder with the given `(name, content)` presets.
    fn folder(tag: &str, files: &[(&str, &str)]) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("workshop-presets-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for (name, content) in files {
            fs::write(dir.join(format!("{name}.toml")), content).unwrap();
        }
        dir
    }

    #[test]
    fn extends_chain_and_value_types() {
        let dir = folder(
            "chain",
            &[
                ("base", "name = \"base\"\nverbose = \"frames\"\n[rgb]\nbrightness = 70\npattern = \"ocean\"\n"),
                ("mid", "extends = \"base\"\nled1 = \"blink 2.5Hz\"\n[rgb]\nbrightness = 10\n"),
                (
                    "leaf",
                    "extends = \"mid\"\nname = \"leaf\"\ndata_period_ms = 12.5\n[net]\naddress = \"10.0.0.1\"\npeer = \"10.0.0.2\"\n",
                ),
            ],
        );
        let p = load(&dir, "leaf", App::Ethernet).unwrap();
        let c = p.config;
        assert_eq!(
            (c.name, c.verbose, c.rgb.brightness, c.rgb.pattern),
            ("leaf", workshop_core::config::Verbosity::Frames, 10, "ocean")
        );
        assert_eq!(c.data_period_ms, 12.5);
        assert_eq!(c.led1, workshop_core::config::LedMode::Blink { period_ms: 400.0 });
        assert_eq!(p.files.len(), 3);
        assert_eq!(c.net().address, [10, 0, 0, 1]);
    }

    #[test]
    fn explicit_period_wins_over_tx_mode_period() {
        let dir = folder("period", &[("p", "tx_mode = \"periodic 2.5Hz\"\ndata_period_ms = 1000\n")]);
        let c = load(&dir, "p", App::Uart).unwrap().config;
        assert_eq!(c.data_period_ms, 1000.0);
        let dir = folder("period2", &[("p", "tx_mode = \"periodic 2.5Hz\"\n")]);
        assert_eq!(load(&dir, "p", App::Uart).unwrap().config.data_period_ms, 400.0);
    }

    #[test]
    fn mistakes_are_reported_with_file_and_key() {
        let dir = folder(
            "errors",
            &[
                ("typo", "led3 = \"on\"\n"),
                ("badvalue", "[rgb]\nbrightness = 170\n"),
                ("badtype", "led1 = [1, 2]\n"),
                ("section", "[colors]\nred = 1\n"),
                ("uartkey", "[pins]\nuart_tx = 5\n"),
                ("phy", "[pins]\nled1 = 9\n"),
                ("subnet", "[net]\npeer = \"10.9.9.9\"\n"),
                ("a", "extends = \"b\"\n"),
                ("b", "extends = \"a\"\n"),
                ("orphan", "extends = \"nowhere\"\n"),
                ("broken", "this is = = not toml\n"),
            ],
        );
        let msg = |name: &str, app| load(&dir, name, app).unwrap_err().0;
        assert!(msg("typo", App::Ethernet).contains("`led3`: unknown setting"));
        // The classic TOML trap: a plain setting after a [section] lands in the section.
        let dir2 = folder("trap", &[("t", "[pins]\nbutton = 4\nled1 = \"peer\"\n")]);
        let trap = load(&dir2, "t", App::Ethernet).unwrap_err().0;
        assert!(
            trap.contains("`pins.led1`") && trap.contains("write plain settings before the first [section]"),
            "{trap}"
        );
        assert!(msg("badvalue", App::Ethernet).contains("`rgb.brightness`"));
        assert!(msg("badtype", App::Ethernet).contains("`led1` must be a string"));
        assert!(msg("section", App::Ethernet).contains("unknown section [colors]"));
        assert!(msg("uartkey", App::Ethernet).contains("only exist on a UART node"));
        assert!(msg("phy", App::Ethernet).contains("GPIO5-19"), "{}", msg("phy", App::Ethernet));
        assert!(msg("subnet", App::Ethernet).contains("not in the subnet"));
        assert!(msg("a", App::Ethernet).contains("cycle"));
        assert!(msg("orphan", App::Ethernet).contains("nowhere.toml"));
        assert!(msg("orphan", App::Ethernet).contains("Available in"));
        assert!(msg("broken", App::Ethernet).contains("not valid TOML"));
        assert!(msg("../x", App::Ethernet).contains("bad preset name"));
        // The PHY pins are fine on a UART node.
        assert!(load(&dir, "phy", App::Uart).is_ok());
    }

    #[test]
    fn generated_source_has_the_whole_config() {
        let dir = folder("source", &[("p", "name = \"node-Z\"\n[net]\ntransport = \"tcp\"\n")]);
        let text = rust_source(&load(&dir, "p", App::Ethernet).unwrap());
        assert!(text.starts_with("// Generated by workshop-presets"));
        assert!(text.contains("use workshop_core::config::*;"));
        assert!(text.contains("name: \"node-Z\""));
        assert!(text.contains("transport: Transport::Tcp"));
        assert!(text.trim_end().ends_with('}'));
    }

    // --- The presets shipped in the repository -------------------------------------------------

    fn repo_presets(app: App) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").join(app.firmware_dir()).join("presets")
    }

    #[test]
    fn every_shipped_preset_loads() {
        for app in [App::Uart, App::Ethernet] {
            let dir = repo_presets(app);
            let names = list(&dir);
            assert!(names.contains(&"base".to_owned()) && names.contains(&"node-a".to_owned()), "{app:?}: {names:?}");
            for name in names {
                load(&dir, &name, app).unwrap_or_else(|e| panic!("{app:?}/{name}: {e}"));
            }
        }
    }

    /// `base.toml` is the documented copy of the defaults: it must say what the defaults are.
    #[test]
    fn base_is_the_documented_default() {
        for app in [App::Uart, App::Ethernet] {
            let base = load(&repo_presets(app), "base", app).unwrap().config;
            let defaults = app.defaults();
            // `name` is the only deliberate difference: the default is "node".
            assert_eq!(
                NodeConfig { name: defaults.name, ..base },
                defaults,
                "{app:?}: base.toml drifted from the defaults"
            );
        }
    }

    /// Every setting is written (and so documented) in `base.toml`.
    #[test]
    fn base_mentions_every_setting() {
        for app in [App::Uart, App::Ethernet] {
            let text = fs::read_to_string(repo_presets(app).join("base.toml")).unwrap();
            let config = app.defaults();
            for (key, _) in describe(&config) {
                let leaf = key.rsplit('.').next().unwrap();
                assert!(
                    text.contains(&format!("{leaf} ")) || text.contains(&format!("{leaf}=")),
                    "{app:?}: `{key}` is not in base.toml"
                );
            }
        }
    }

    #[test]
    fn node_a_and_node_b_are_a_pair() {
        let (a, b) = (
            load(&repo_presets(App::Ethernet), "node-a", App::Ethernet).unwrap().config,
            load(&repo_presets(App::Ethernet), "node-b", App::Ethernet).unwrap().config,
        );
        assert_ne!(a.name, b.name);
        assert_eq!((a.net().address, a.net().peer), (b.net().peer, b.net().address), "each is the other's peer");
        assert_eq!((a.net().transport, a.net().port), (b.net().transport, b.net().port));
        let tcp = |n: &str| load(&repo_presets(App::Ethernet), n, App::Ethernet).unwrap().config.net();
        assert_ne!(tcp("node-a-tcp").tcp_role, tcp("node-b-tcp").tcp_role, "one server, one client");
    }
}

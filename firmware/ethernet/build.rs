fn main() {
    linker_be_nice();
    // make sure linkall.x is the last linker script (otherwise might cause problems with flip-link)
    println!("cargo:rustc-link-arg=-Tlinkall.x");
    build_info();
    presets();
}

/// Turns the TOML presets of each node into `$OUT_DIR/node_a.rs` / `node_b.rs`, which the
/// binaries `include!` as their `NodeConfig`. A bad preset stops the build with a message
/// naming the file and the key (see docs/book/presets.md).
///
/// The preset of each binary is `node-a` / `node-b`, unless chosen with an environment variable:
/// `PRESET_NODE_A`, `PRESET_NODE_B`, or `PRESET` for both (e.g. `PRESET=node-a-tcp`).
fn presets() {
    use std::{env, path::Path};
    use workshop_presets::{App, generate_for_build};

    let manifest = env::var("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR");
    let out = env::var("OUT_DIR").expect("cargo sets OUT_DIR");
    let dir = Path::new(&manifest).join("presets");
    for var in ["PRESET", "PRESET_NODE_A", "PRESET_NODE_B"] {
        println!("cargo:rerun-if-env-changed={var}");
    }
    let chosen = |specific: &str, default: &str| {
        env::var(specific)
            .or_else(|_| env::var("PRESET"))
            .ok()
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| default.to_owned())
    };
    for (file, var, default) in [("node_a.rs", "PRESET_NODE_A", "node-a"), ("node_b.rs", "PRESET_NODE_B", "node-b")] {
        generate_for_build(App::Ethernet, &dir, &chosen(var, default), &Path::new(&out).join(file));
    }
}

/// Exposes build metadata to the firmware as `env!("BUILD_*")` (shown in the boot banner).
fn build_info() {
    use std::{env, process::Command};

    // Refresh the info (build date, git state) whenever sources or dependencies change.
    for path in ["src", "Cargo.toml", "Cargo.lock", "../../.git/HEAD", "../../.git/index"] {
        println!("cargo:rerun-if-changed={path}");
    }

    let run = |cmd: &str, args: &[&str]| -> Option<String> {
        let out = Command::new(cmd).args(args).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let set = |key: &str, value: &str| println!("cargo:rustc-env={key}={value}");

    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    set("BUILD_RUSTC", run(&rustc, &["-V"]).as_deref().unwrap_or("?").trim_start_matches("rustc "));
    set("BUILD_CARGO", run(&cargo, &["-V"]).as_deref().unwrap_or("?").trim_start_matches("cargo "));

    let git = match run("git", &["rev-parse", "--short", "HEAD"]) {
        Some(hash) => {
            let dirty = run("git", &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
            let branch = run("git", &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
            format!("{hash} ({branch}){}", if dirty { " +dirty" } else { "" })
        }
        None => "no commit yet".into(),
    };
    set("BUILD_GIT", &git);

    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    set("BUILD_DATE", &format_utc(secs));

    for (key, var) in [
        ("BUILD_PROFILE", "PROFILE"),
        ("BUILD_OPT_LEVEL", "OPT_LEVEL"),
        ("BUILD_TARGET", "TARGET"),
        ("BUILD_HOST", "HOST"),
    ] {
        set(key, &env::var(var).unwrap_or_else(|_| "?".into()));
    }

    let manifest = std::fs::read_to_string("Cargo.toml").unwrap_or_default();
    let edition = manifest
        .lines()
        .find_map(|l| {
            l.trim().strip_prefix("edition")?.trim().strip_prefix('=').map(|v| v.trim().trim_matches('"').to_string())
        })
        .unwrap_or_else(|| "?".into());
    set("BUILD_EDITION", &edition);

    // Resolved versions of the main dependencies, from Cargo.lock. A crate can appear
    // several times (e.g. heapless 0.8 pulled by esp-hal, 0.9 used here): Cargo.lock sorts
    // them by version, so the last entry is the newest, which is the one this project uses.
    let lock = std::fs::read_to_string("Cargo.lock").unwrap_or_default();
    let version_of = |name: &str| {
        let needle = format!("name = \"{name}\"\nversion = \"");
        let start = lock.rfind(&needle)? + needle.len();
        Some(lock[start..].split('"').next()?.to_string())
    };
    let deps: Vec<String> = ["esp-hal", "esp-rtos", "embassy-net", "esp-println", "esp-backtrace", "heapless"]
        .iter()
        .filter_map(|name| Some(format!("{name} {}", version_of(name)?)))
        .collect();
    set("BUILD_DEPS", &deps.join(" · "));
}

/// Formats a Unix timestamp as `YYYY-MM-DD HH:MM:SS UTC` (no external crate).
fn format_utc(secs: u64) -> String {
    let (days, rem) = (secs / 86_400, secs % 86_400);
    // Civil-from-days algorithm (Howard Hinnant), valid for any date after 1970.
    let z = days as i64 + 719_468;
    let era = z / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC", rem / 3600, rem % 3600 / 60, rem % 60)
}

fn linker_be_nice() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 {
        let kind = &args[1];
        let what = &args[2];

        match kind.as_str() {
            "undefined-symbol" => match what.as_str() {
                what if what.starts_with("_defmt_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `defmt` not found - make sure `defmt.x` is added as a linker script and you have included `use defmt_rtt as _;`"
                    );
                    eprintln!();
                }
                "_stack_start" => {
                    eprintln!();
                    eprintln!("💡 Is the linker script `linkall.x` missing?");
                    eprintln!();
                }
                what if what.starts_with("esp_rtos_") => {
                    eprintln!();
                    eprintln!(
                        "💡 `esp-radio` has no scheduler enabled. Make sure you have initialized `esp-rtos` or provided an external scheduler."
                    );
                    eprintln!();
                }
                "embedded_test_linker_file_not_added_to_rustflags" => {
                    eprintln!();
                    eprintln!(
                        "💡 `embedded-test` not found - make sure `embedded-test.x` is added as a linker script for tests"
                    );
                    eprintln!();
                }
                "free"
                | "malloc"
                | "calloc"
                | "get_free_internal_heap_size"
                | "malloc_internal"
                | "realloc_internal"
                | "calloc_internal"
                | "free_internal" => {
                    eprintln!();
                    eprintln!(
                        "💡 Did you forget the `esp-alloc` dependency or didn't enable the `compat` feature on it?"
                    );
                    eprintln!();
                }
                _ => (),
            },
            // we don't have anything helpful for "missing-lib" yet
            _ => {
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }

    println!("cargo:rustc-link-arg=--error-handling-script={}", std::env::current_exe().unwrap().display());
}

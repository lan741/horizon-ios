//! The iOS app's entry: UIKit hands the app its folders, and the game runs in this
//! process (docs/ios.md).
//!
//! iOS has no fork and no exec, so the two things a desktop start does are gone here:
//! no child command is inspected for its arguments, and no install lease is taken from
//! one. It also has no dynamic loading: `horizon-mod-host` would open native libraries
//! and start a WebAssembly runtime that writes and executes code pages, which the
//! platform refuses. This entry therefore does the part of `horizon-mod-host::main_with`
//! the engine needs — the engine's slot provider, the mod instrumentation and the render
//! points — and skips the code loading.
//!
//! Everything after that is one engine start: [`horizon_client::entry::start`] installs
//! the hooks, opens the event loop and runs the game until the player quits.
#![deny(unsafe_code)]

use std::ffi::{CStr, OsString, c_char, c_int};
use std::path::PathBuf;

use horizon_mod_package::runtime::Runtime;

/// `rustc` for the crash report and `horizon-ctl status`, as the desktop host reports it.
const RUSTC_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Why the game stopped, for `main.m`'s exit code (0 = the player quit).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    Failed = 1,
}

/// The folders a sandboxed app runs with, as NUL-terminated UTF-8 paths.
///
/// `documents` holds the game folder (saves, options.txt, packs) and, unless `resources`
/// names another, the prepared resources; `support` holds logs and crash reports, which
/// iOS keeps out of the user's visible files.
#[repr(C)]
pub struct Paths {
    pub documents: *const c_char,
    pub support: *const c_char,
    /// Prepared resources (`horizon-resources.json` inside); null uses
    /// `<documents>/horizon-resources`.
    pub resources: *const c_char,
}

/// Runs the game until the player quits. Called from `main` on the main thread: winit's
/// iOS backend owns `UIApplicationMain` from here, so nothing may create a `UIWindow`
/// before it.
///
/// # Safety
/// Each pointer in `paths` is NUL-terminated UTF-8 that outlives the call, or null.
#[allow(unsafe_code, reason = "the C entry point and the host's path pointers")]
#[unsafe(no_mangle)]
pub unsafe extern "C" fn horizon_ios_run(paths: *const Paths) -> c_int {
    // SAFETY: the caller guarantees `paths` is a live `Paths` for this call.
    let paths = unsafe { paths.as_ref() };
    let documents = paths.and_then(|p| path(p.documents)).unwrap_or_else(home_fallback);
    let support = paths.and_then(|p| path(p.support)).unwrap_or_else(|| documents.clone());
    let resources = paths
        .and_then(|p| path(p.resources))
        .unwrap_or_else(|| documents.join("horizon-resources"));
    match run(&documents, &support, &resources) {
        Status::Ok => 0,
        Status::Failed => 1,
    }
}

/// The game's start for these folders, as the argument vector a launcher would have built.
fn run(
    documents: &std::path::Path,
    support: &std::path::Path,
    resources: &std::path::Path,
) -> Status {
    let home = support.join("horizon");
    if let Err(error) = std::fs::create_dir_all(&home) {
        eprintln!("Horizon: cannot create {}: {error}", home.display());
        return Status::Failed;
    }
    let args: Vec<OsString> = vec![
        "--game-dir".into(),
        documents.as_os_str().to_owned(),
        "--home".into(),
        home.as_os_str().to_owned(),
        "--resources".into(),
        resources.as_os_str().to_owned(),
    ];
    // A bad command line ends the process inside `prepare_from`, as on a desktop.
    let startup = horizon_client::entry::prepare_from(args, Some(documents));
    install();
    horizon_client::entry::start(
        // No compiled-in native mods: the app has no `horizon-mods` folder to load them
        // from, and `Runtime::load` would refuse the missing packages (docs/ios.md).
        Runtime::default(),
        |_, _| (Vec::new(), ()),
        horizon_client::entry::CodeRuntime {
            initialize: no_code_initialize,
            refusal: no_code_refusal,
            startup_refused: no_code_startup_refusal,
        },
        startup,
    );
    Status::Ok
}

/// `horizon-mod-host::main_with`'s engine setup without its code loading: the slot
/// provider, the instrumentation, the platform half of the slots and the render points.
/// Each installed once, so a second call would be the host's own mistake.
fn install() {
    horizon_engine_slots::install_seal(mod_points::__private::seal)
        .expect("engine seal provider must be installed once before startup");
    if let Err(error) = mod_api::__private::initialize() {
        eprintln!("Horizon: refusing startup because mod instrumentation is unavailable: {error}");
        std::process::exit(1);
    }
    if let Err(error) = horizon_engine_slots::install_platform(horizon_engine_slots::Platform {
        install_packages: mod_api::__private::install_packages,
        install_registration_data: mod_api::__private::install_registration_data,
        install_parts: mod_api::__private::install_parts,
        profiler_is_enabled: mod_api::__private::profiler::is_enabled,
        profiler_start: mod_api::__private::profiler::start,
        profiler_stop: mod_api::__private::profiler::stop,
        profiler_summary: mod_api::__private::profiler::summary,
        profiler_firefox_profile: mod_api::__private::profiler::firefox_profile,
    }) {
        eprintln!("Horizon: {error}");
        std::process::exit(1);
    }
    mod_render::render::install();
}

fn no_code_initialize(_: &[mod_api::PackageInfo]) {}
fn no_code_refusal(_: &str) -> Option<String> {
    None
}
fn no_code_startup_refusal(_: &str) -> bool {
    false
}

/// The build string the crash report and `horizon-ctl status` show.
pub fn rustc_version() -> &'static str {
    RUSTC_VERSION
}

/// A path the host passed, or nothing when it passed none.
#[allow(unsafe_code, reason = "reads the host's NUL-terminated string")]
fn path(raw: *const c_char) -> Option<PathBuf> {
    if raw.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees a NUL-terminated string for the whole call.
    let text = unsafe { CStr::from_ptr(raw) }.to_str().ok()?;
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// No `Paths` at all (a host that has not been updated): `$HOME`, which the sandbox
/// sets to the app container.
fn home_fallback() -> PathBuf {
    std::env::var_os("HOME").map_or_else(|| PathBuf::from("."), PathBuf::from)
}
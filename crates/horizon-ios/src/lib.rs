//! The iOS app's entry: UIKit hands the app its folders, and the game runs in this
//! process (docs/ios.md).
//!
//! iOS has no fork and no exec, so the two things a desktop start does are gone here:
//! no child command is inspected for its arguments, and no install lease is taken from
//! one. Everything after [`horizon_client::entry::prepare_from`] is the same engine
//! start a desktop run makes: `entry::start` installs the hooks, opens the event loop
//! and runs the game until the player quits.
//!
//! The mod host is left out on purpose: it loads native libraries and a WebAssembly
//! runtime that write and execute code pages, which iOS refuses (`MAP_JIT` and the
//! code-signing rules). Content mods read from a folder still load through the engine.
#![deny(unsafe_code)]

use std::ffi::{CStr, OsString, c_char, c_int};
use std::path::PathBuf;

use horizon_mod_package::runtime::Runtime;

/// Why the game stopped, for `main.m`'s exit code (0 = the player quit).
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok = 0,
    Usage = 2,
}

/// The folders a sandboxed app runs with, as NUL-terminated UTF-8 paths.
///
/// `documents` holds the game folder (saves, options.txt, packs) and the prepared
/// resources; `support` holds logs and crash reports, which iOS keeps out of the
/// user's visible files.
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
        Status::Usage => 2,
    }
}

/// The game's start for these folders: the argument vector a launch would have had.
fn run(documents: &std::path::Path, support: &std::path::Path, resources: &std::path::Path) -> Status {
    let home = support.join("horizon");
    if let Err(error) = std::fs::create_dir_all(&home) {
        log::error!("Horizon: cannot create {}: {error}", home.display());
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
    // No compiled-in native mods and no code runtime: iOS loads neither, and the
    // engine's own content still comes from the game folder (docs/ios.md).
    horizon_client::entry::start(
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

fn no_code_initialize(_: &[horizon_mod_vocab::PackageInfo]) {}
fn no_code_refusal(_: &str) -> Option<String> {
    None
}
fn no_code_startup_refusal(_: &str) -> bool {
    false
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
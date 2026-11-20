//! The launch log an iOS build writes, and the hooks that make a crash legible.
//!
//! On iOS a panic and a signal both end the process at once, and the log is the only
//! record a player can reach: `stdout`/`stderr` go nowhere they can see. This module
//! appends one line per startup step to `Documents/horizon-launch.log` (the file `main.m`
//! opens and names in `HORIZON_IOS_LOG`), records panics with their location and a
//! backtrace, and records the signal that killed the app with its fault address.
#![allow(unsafe_code, reason = "the signal handler writes with libc's write, the only async-signal-safe call")]

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI32, Ordering};

/// The log `main.m` opened; `None` when the host named none (a test).
static LOG_PATH: OnceLock<Option<PathBuf>> = OnceLock::new();
/// The same file as a raw descriptor, for the signal handler.
static RAW_FD: AtomicI32 = AtomicI32::new(-1);

/// Resolves the log path and keeps a descriptor for the signal handler.
pub fn init() {
    let path = std::env::var_os("HORIZON_IOS_LOG").map(PathBuf::from);
    if let Some(path) = &path
        && let Ok(file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
    {
        use std::os::fd::IntoRawFd;
        // Leaked on purpose: the descriptor outlives every Rust value here.
        RAW_FD.store(file.into_raw_fd(), Ordering::Release);
    }
    let _ = LOG_PATH.set(path);
}

/// One startup step, flushed at once so a crash right after it leaves it in the file.
pub fn step(name: &str) {
    let Some(Some(path)) = LOG_PATH.get() else {
        eprintln!("step: {name}");
        return;
    };
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        eprintln!("step: {name}");
        return;
    };
    let _ = writeln!(file, "step: {name}");
    let _ = file.flush();
}

/// A panic: its message, its location and a backtrace, then the abort iOS would do anyway.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown location".into());
        let message = payload(info.payload());
        let backtrace = std::backtrace::Backtrace::force_capture();
        if let Some(Some(path)) = LOG_PATH.get()
            && let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(path)
        {
            let _ = writeln!(file, "PANIC at {location}: {message}");
            let _ = writeln!(file, "{backtrace}");
            let _ = file.flush();
        }
        previous(info);
    }));
}

fn payload(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(text) = payload.downcast_ref::<&str>() {
        return (*text).to_owned();
    }
    if let Some(text) = payload.downcast_ref::<String>() {
        return text.clone();
    }
    "non-string panic payload".to_owned()
}

/// The signals a broken start raises on iOS: a bad pointer, a stack overflow, an abort.
/// Each handler writes one line and lets the default disposition kill the app, so the
/// crash report the OS writes still says what happened.
pub fn install_signal_handlers() {
    for signal in [libc::SIGSEGV, libc::SIGBUS, libc::SIGABRT, libc::SIGILL, libc::SIGFPE] {
        // SAFETY: `handle` only calls `write` on a descriptor opened at init, and every
        // other field of the action is a default or a null pointer.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = handle as usize;
            action.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
}

/// Writes `SIG <n> addr <hex>` and returns, so the default action follows.
extern "C" fn handle(signal: libc::c_int, info: *mut libc::siginfo_t, _: *mut libc::c_void) {
    let fd = RAW_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }
    let mut line = [0u8; 64];
    let mut len = 0;
    let mut push = |byte: u8| {
        if len < line.len() {
            line[len] = byte;
            len += 1;
        }
    };
    for byte in b"SIG " {
        push(*byte);
    }
    write_unsigned(&mut push, signal as u64, 10);
    for byte in b" addr 0x" {
        push(*byte);
    }
    // SAFETY: a live `siginfo_t` from the kernel while this handler runs.
    let address = unsafe { (*info).si_addr() } as u64;
    write_unsigned(&mut push, address, 16);
    push(b'\n');
    // SAFETY: `line[..len]` is this thread's buffer and `fd` is the log's descriptor.
    unsafe {
        libc::write(fd, line.as_ptr().cast(), len);
    }
}

/// Decimal or hexadecimal digits of `value`, least significant first, reversed.
fn write_unsigned(push: &mut impl FnMut(u8), mut value: u64, base: u64) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut scratch = [0u8; 16];
    let mut len = 0;
    if value == 0 {
        push(b'0');
        return;
    }
    while value > 0 {
        scratch[len] = DIGITS[(value % base) as usize];
        len += 1;
        value /= base;
    }
    while len > 0 {
        len -= 1;
        push(scratch[len]);
    }
}
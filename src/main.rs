mod proc_utils;
mod program_tray;
mod proc_monitor;

use crate::proc_monitor::start_proc_connector;
use crate::program_tray::update_tray;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{Thread, park};

static STEAM_CLIENT_PID: AtomicU32 = AtomicU32::new(0);
static WEBHELPER_PID: AtomicU32 = AtomicU32::new(0);

static GAME_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static GAME_OVERLAY_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static FROZEN_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static DISABLE_CEF_ON_GAMING: AtomicBool = AtomicBool::new(true);
static ACTIVATED_GAME_MODE: AtomicBool = AtomicBool::new(false);

static MAIN_THREAD: Mutex<Option<Thread>> = Mutex::new(None);

#[macro_export] macro_rules! log {
    ($($arg:tt)*) => {
        println!("[no-steam-webhelper] {}", format_args!($($arg)*));
    };
}

#[macro_export] macro_rules! err {
    ($($arg:tt)*) => {
        println!("[no-steam-webhelper] {}", format_args!($($arg)*));
    };
}

fn main() {
    let mut guard = MAIN_THREAD.lock().unwrap();
    *guard = Some(std::thread::current());
    drop(guard);
    start_proc_connector();
    launch_steam();

    if let Err(err) = ctrlc::set_handler(move || {
        log!("Caught exit signal");
        continue_main();
    }) {
        err!("Failed to set ctrlc handler: {}", err);
    }

    park();
    cleanup();
    log!("Program exits");
}

fn launch_steam() {
    let child = Command::new("/usr/bin/steam").args(std::env::args().skip(1)).spawn().expect("Failed to launch steam");
    log!("Launching steam launcher script, pid {}", child.id());
}

fn continue_main() {
    if let Some(thread) = MAIN_THREAD.lock().unwrap().as_ref() {
        thread.unpark();
    }
}

fn kill_cef() {
    let webhelper_pid = WEBHELPER_PID.load(Ordering::Relaxed);
    if webhelper_pid != 0 {
        unsafe { libc::kill(webhelper_pid as i32, libc::SIGKILL); }
        // ProcConnector will capture the proc exit, no need to set to zero
    }
}

fn term_steam() {
    let pid = STEAM_CLIENT_PID.load(Ordering::Relaxed);
    if pid != 0 {
        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    }
}

fn cleanup() {
    log!("Running cleanup");
    continue_frozen();
    term_steam()
}

fn is_no_game_process() -> bool {
    GAME_PIDS.lock().unwrap().is_empty()
}

fn on_game_start() {
    if DISABLE_CEF_ON_GAMING.load(Ordering::Relaxed) && start_game_mode() {
        update_tray()
    }
}

fn on_game_exit() {
    if is_no_game_process() && stop_game_mode() {
        update_tray()
    }
}

fn start_game_mode() -> bool {
    if ACTIVATED_GAME_MODE.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Starting game mode");
        kill_cef();
        GAME_OVERLAY_PIDS.lock().unwrap().iter().for_each(|pid| {
            freeze_pid(*pid);
        });
        true
    } else { false }
}

fn stop_game_mode() -> bool {
    if ACTIVATED_GAME_MODE.compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Exiting game mode");
        continue_frozen();
        true
    } else { false }
}

fn freeze_pid(pid: u32) {
    let mut frozen = FROZEN_PIDS.lock().unwrap();
    unsafe {
        libc::kill(pid as i32, libc::SIGSTOP);
    }
    frozen.push(pid);
    log!("Freeze pid {pid}");
}

fn continue_frozen() {
    let mut frozen = FROZEN_PIDS.lock().unwrap();
    frozen.iter().for_each(|&pid| {
        unsafe { libc::kill(pid as i32, libc::SIGCONT); }
        log!("Continue pid {pid}");
    });
    frozen.clear();
}

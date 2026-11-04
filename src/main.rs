use proc_connector::{EventMask, ProcConnector, ProcEvent};
use std::collections::HashSet;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{Thread, park};

static STEAM_ROOT_PID: AtomicU32 = AtomicU32::new(0);
static STEAM_CLIENT_PID: AtomicU32 = AtomicU32::new(0);
static WEBHELPER_PID: AtomicU32 = AtomicU32::new(0);
static GAME_OVERLAY_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static FROZEN_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static GAME_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static DISABLE_CEF_ON_GAME_MODE: AtomicBool = AtomicBool::new(true);
static BLOCKING_WEBHELPER: AtomicBool = AtomicBool::new(false);

static MAIN_THREAD: Mutex<Option<Thread>> = Mutex::new(None);
static TRAY_ICON: Mutex<Option<ksni::blocking::Handle<ProgramTray>>> = Mutex::new(None);

struct ProgramTray;
impl ProgramTray {
    fn new() -> Self {
        Self {}
    }
}

impl ksni::Tray for ProgramTray {
    fn id(&self) -> String {
        env!("CARGO_PKG_NAME").into()
    }
    fn title(&self) -> String {
        "Steam-NoSteamWebHelper".into()
    }
    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        use ksni::Icon;
        fn create_icon(size: u32, color: [u8; 4]) -> Icon {
            let mut data = Vec::with_capacity((size * size * 4) as usize);
            let center = size as f32 / 2.0;
            let radius = center * 0.6;

            for y in 0..size {
                for x in 0..size {
                    let dx = x as f32 + 0.5 - center;
                    let dy = y as f32 + 0.5 - center;
                    let distance = (dx * dx + dy * dy).sqrt();

                    let alpha = ((radius + 0.5 - distance).clamp(0.0, 1.0)
                        * color[3] as f32) as u8;

                    data.extend_from_slice(&[alpha, color[0], color[1], color[2]]);
                }
            }

            Icon {
                width: size as i32,
                height: size as i32,
                data,
            }
        }
        let color = if BLOCKING_WEBHELPER.load(Ordering::Relaxed) {
            [46, 204, 113, 255]
        } else if DISABLE_CEF_ON_GAME_MODE.load(Ordering::Relaxed) {
            [241, 196, 15, 255]
        } else {
            [149, 165, 166, 255]
        };
        vec![
            create_icon(64, color),
            create_icon(32, color),
        ]
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::*;
        vec![
            CheckmarkItem {
                label: "Disable CEF On Gaming".into(),
                checked: DISABLE_CEF_ON_GAME_MODE.load(Ordering::Relaxed),
                activate: Box::new(|_| update_tray_options()),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Restart CEF".into(),
                activate: Box::new(|_| kill_cef()),
                ..Default::default()
            }.into(),
            StandardItem {
                label: "Exit Steam".into(),
                activate: Box::new(|_| term_steam()),
                ..Default::default()
            }.into(),
        ]
    }
}

macro_rules! log {
    ($($arg:tt)*) => {
        println!("[no-steam-webhelper] {}", format_args!($($arg)*));
    };
}

macro_rules! err {
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

fn start_proc_connector() {
    let conn = ProcConnector::new().expect("failed to create process connector");
    if let Err(e) = conn.subscribe_filtered(EventMask::EXEC | EventMask::EXIT) {
        err!("Failed to set EventMask: {}", e);
    }
    let mut buf = vec![0u8; 4096];

    let mut launching_pids: HashSet<u32> = HashSet::new();
    launching_pids.insert(std::process::id());
    let mut steam_client_pid = 0u32; // no-steam-webhelper -> bash -> steam -> srt-client-container -> srt-bwrap -> pv-adverb -> steam
    let mut steam_launcher_pid = 0u32;

    std::thread::spawn(move || {
        // Catch steam pid
        loop {
            match conn.recv(&mut buf) {
                Ok(event) => {
                    match event {
                        ProcEvent::Exec { pid, .. } => {
                            if let Some(ppid) = get_proc_parent(pid) {
                                if launching_pids.contains(&ppid) {
                                    launching_pids.insert(pid);
                                    if let Some(comm) = get_proc_comm(pid) {
                                        match comm.as_str() {
                                            "x86_64-linux-gn" /* x86_64-linux-gnu-srt-launcher-service */ => {
                                                if steam_launcher_pid == 0 {
                                                    steam_launcher_pid = pid;
                                                    log!("steam_launcher_pid is {}", pid);
                                                }
                                            }
                                            "steam" => {
                                                if let Some(ppid) = get_proc_parent(pid) && get_proc_comm(ppid) == Some("pv-adverb".to_string()) {
                                                    if steam_client_pid == 0 {
                                                        steam_client_pid = pid;
                                                        STEAM_CLIENT_PID.store(pid, Ordering::Relaxed);
                                                        log!("steam_client_pid is {}", pid);
                                                        break
                                                    }
                                                }
                                            }
                                            _ => {}
                                        }
                                    }
                                }
                            }
                        }
                        ProcEvent::Exit { pid, .. } => {
                            if launching_pids.remove(&pid) && launching_pids.len() == 1 {
                                // All steam processes exited
                                continue_main();
                                return;
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => { err!("error: {e:?}"); }
            }
        }
        drop(launching_pids);
        start_tray();
        // Now capture game running
        loop {
            match conn.recv(&mut buf) {
                Ok(event) => {
                    match event {
                        ProcEvent::Exec { pid, .. } => {
                            if let Some(ppid) = get_proc_parent(pid) {
                                if ppid == steam_launcher_pid {
                                    if get_proc_comm(pid).map_or(false, |s| s.contains("steam-runtime")) {
                                        GAME_PROC.lock().unwrap().push(pid);
                                        on_game_start();
                                        log!("Game {pid} started");
                                    }
                                } else if ppid == steam_client_pid {
                                    match get_proc_comm(pid).unwrap().as_str() {
                                        "steamwebhelper" => {
                                            if BLOCKING_WEBHELPER.load(Ordering::Relaxed) {
                                                unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                                            } else {
                                                // We only store the root process of steam web helper
                                                // Term the root process kills all of them
                                                WEBHELPER_PID.store(pid, Ordering::Relaxed);
                                                log!("webhelper_pid is {pid}");
                                            }
                                        }
                                        "gameoverlayui" => {
                                            GAME_OVERLAY_PROC.lock().unwrap().push(pid);
                                            log!("gameoverlayui {pid} started");
                                            if BLOCKING_WEBHELPER.load(Ordering::Relaxed) {
                                                freeze_pid(pid)
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        ProcEvent::Exit { parent_tgid, pid, .. } => {
                            fn rm_elem(pids: &Mutex<Vec<u32>>, pid: u32) -> bool {
                                let mut vec = pids.lock().unwrap();
                                if let Some(pos) = vec.iter().position(|&x| x == pid) {
                                    vec.remove(pos);
                                    true
                                } else {
                                    false
                                }
                            }
                            if parent_tgid == steam_client_pid {
                                if pid == WEBHELPER_PID.load(Ordering::Relaxed) {
                                    WEBHELPER_PID.store(0, Ordering::Relaxed);
                                    log!("webhelper_pid {pid} exited");
                                } else if rm_elem(&GAME_OVERLAY_PROC, pid) {
                                    log!("gameoverlayui {pid} exited");
                                }
                            } else if parent_tgid == steam_launcher_pid {
                                if rm_elem(&GAME_PROC, pid) {
                                    log!("Game {pid} exited");
                                    on_game_exit();
                                }
                            } else if pid == steam_client_pid {
                                log!("Steam client {pid} exited");
                                continue_main();
                                return;
                            }
                        }
                        _ => {}
                    }
                }
                Err(e) => { err!("error: {e:?}"); }
            }
        }
    });
}

fn start_tray() {
    std::thread::spawn(|| {
        use ksni::blocking::TrayMethods;
        if let Ok(tray) = ProgramTray::new().spawn() {
            let mut guard = TRAY_ICON.lock().unwrap();
            *guard = Some(tray);
        }
    });
}

fn update_tray() {
    if let Some(tray) = TRAY_ICON.lock().unwrap().as_ref() {
        tray.update(|_| {}); // Update tray icon
    }
}

fn get_proc_parent(pid: u32) -> Option<u32> {
    let path = format!("/proc/{}/stat", pid);
    let content = std::fs::read_to_string(path).ok()?;
    let ppid_start = content.rfind(')')? + 4;
    let ppid = &content[ppid_start..];
    let ppid_end = ppid.find(' ')?;
    ppid[..ppid_end].parse().ok()
}

// fn get_proc_exe(pid: u32) -> Option<PathBuf> {
//     let path = format!("/proc/{}/exe", pid);
//     std::fs::read_link(path).ok()
// }
//
// fn get_exe_path(path: &Option<PathBuf>) -> Option<String> {
//     path.as_deref().and_then(|p| p.to_str()).map(String::from)
// }
//
// fn matches_file_name(path: &PathBuf, name: &str) -> bool {
//     path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n == name)
// }
//
// fn get_proc_cmdline(pid: u32) -> Option<String> {
//     let path = format!("/proc/{}/cmdline", pid);
//     std::fs::read_to_string(path).ok()
// }

fn get_proc_comm(pid: u32) -> Option<String> {
    let path = format!("/proc/{}/comm", pid);
    std::fs::read_to_string(path).map(|s| s.trim().to_string()).ok()
}

fn launch_steam() {
    let child = Command::new("/usr/bin/steam").args(std::env::args().skip(1)).spawn().expect("Failed to launch steam");
    STEAM_ROOT_PID.store(child.id(), Ordering::Relaxed);
    log!("Launching steam steam pid {}", child.id());
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

fn on_game_start() {
    if DISABLE_CEF_ON_GAME_MODE.load(Ordering::Relaxed) && start_game_mode() {
        update_tray()
    }
}

fn on_game_exit() {
    if is_no_game_process() && stop_game_mode() {
        update_tray()
    }
}

fn is_no_game_process() -> bool {
    GAME_PROC.lock().unwrap().is_empty()
}

fn start_game_mode() -> bool {
    if BLOCKING_WEBHELPER.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Starting game mode");
        kill_cef();
        GAME_OVERLAY_PROC.lock().unwrap().iter().for_each(|pid| {
            freeze_pid(*pid);
        });
        true
    } else { false }
}

fn stop_game_mode() -> bool {
    if BLOCKING_WEBHELPER.compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Exiting game mode");
        continue_frozen();
        true
    } else { false }
}

fn update_tray_options() {
    let old = DISABLE_CEF_ON_GAME_MODE.fetch_xor(true, Ordering::Relaxed);
    if old {
        stop_game_mode();
    } else if !is_no_game_process() {
        start_game_mode();
    }
}

fn freeze_pid(pid: u32) {
    unsafe {
        libc::kill(pid as i32, libc::SIGSTOP);
    }
    FROZEN_PROC.lock().unwrap().push(pid);
    log!("Freeze pid {pid}");
}

fn continue_frozen() {
    let mut frozen = FROZEN_PROC.lock().unwrap();
    frozen.iter().for_each(|&pid| {
        unsafe { libc::kill(pid as i32, libc::SIGCONT); }
        log!("Continue pid {pid}");
    });
    frozen.clear();
}

use proc_connector::{EventMask, ProcConnector, ProcEvent};
use std::collections::HashSet;
use std::process;
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::thread::{Thread, park};

static STEAM_ROOT_PID: AtomicU32 = AtomicU32::new(0);
static STEAM_CLIENT_PID: AtomicU32 = AtomicU32::new(0);
static WEBHELPER_PID: AtomicU32 = AtomicU32::new(0);

static FROZEN_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static GAME_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());

static BLOCK_WEBHELPER: AtomicBool = AtomicBool::new(false);

static MAIN_THREAD: Mutex<Option<Thread>> = Mutex::new(None);

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
    {
        let mut guard = MAIN_THREAD.lock().unwrap();
        *guard = Some(std::thread::current());
    }
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
    launching_pids.insert(process::id());
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
                                                        drop(launching_pids);
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
                                    if get_proc_comm(pid).map_or(false, |s| s.eq("steamwebhelper")) {
                                        if BLOCK_WEBHELPER.load(Ordering::Relaxed) {
                                            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                                        } else {
                                            // We only store the root process of steam web helper
                                            // Term the root process kills all of them
                                            WEBHELPER_PID.store(pid, Ordering::Relaxed);
                                            log!("webhelper_pid is {}", pid);
                                        }
                                    }
                                }
                            }
                        }
                        ProcEvent::Exit { parent_pid, pid, .. } => {
                            fn rm_elem(pids: &Mutex<Vec<u32>>, pid: u32) -> bool {
                                let mut vec = pids.lock().unwrap();
                                if let Some(pos) = vec.iter().position(|&x| x == pid) {
                                    vec.remove(pos);
                                    true
                                } else {
                                    false
                                }
                            }
                            if parent_pid == steam_client_pid && pid == WEBHELPER_PID.load(Ordering::Relaxed) {
                                WEBHELPER_PID.store(0, Ordering::Relaxed);
                                log!("webhelper_pid {pid} exited");
                            } else if parent_pid == steam_launcher_pid {
                                if rm_elem(&GAME_PROC, pid) {
                                    on_game_exit();
                                    log!("Game {pid} exited");
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
    if let Some(thread) = MAIN_THREAD.lock().unwrap().clone() {
        thread.unpark();
    }
}

fn cleanup() {
    log!("Running cleanup");
    continue_frozen();
    let pid = STEAM_CLIENT_PID.load(Ordering::Relaxed);
    if pid != 0 {
        unsafe { libc::kill(pid as i32, libc::SIGTERM); }
    }
}

fn on_game_start() {
    if BLOCK_WEBHELPER.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Starting game mode");
        let webhelper_pid = WEBHELPER_PID.load(Ordering::Relaxed);
        unsafe { libc::kill(webhelper_pid as i32, libc::SIGTERM); }
        // ProcConnector will capture the proc exit
    }
}

fn on_game_exit() {
    if GAME_PROC.lock().unwrap().is_empty() && BLOCK_WEBHELPER.compare_exchange(true, false, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        log!("Exiting game mode");
        continue_frozen()
    }
}

fn continue_frozen() {
    let mut wh = FROZEN_PROC.lock().unwrap();
    wh.iter().for_each(|&pid| {
        unsafe { libc::kill(pid as i32, libc::SIGCONT); }
    });
    wh.clear();
}

// fn kwin_close_window(pid: u32) {
//     let script = format!(
//         r#"
//             const targetPid = {pid};
//             const wins = workspace.windowList();
//
//             for (const win of wins) {{
//                 if (win.pid === targetPid && win.closeable) {{
//                     win.closeWindow();
//                 }}
//             }}
//         "#
//     );
//     let script_name = format!("no-steam-webhelper-close-script-tmp-{pid}");
//
//     let path = std::env::temp_dir().join("no-steam-webhelper-close-script.tmp.js");
//     std::fs::write(&path, script).unwrap();
//
//     let conn = dbus::blocking::Connection::new_session().unwrap();
//     let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(10));
//     let script_id: i32;
//     (script_id,) = scripting.method_call("org.kde.kwin.Scripting", "loadScript", (path.to_string_lossy(), &script_name)).unwrap_or((-555,));
//     if script_id < 0 {
//         err!("Failed to loadScript");
//         return;
//     }
//     let loaded = conn.with_proxy(
//         "org.kde.KWin",
//         format!("/Scripting/Script{script_id}"),
//         Duration::from_secs(10),
//     );
//     let _: () = loaded.method_call("org.kde.kwin.Script", "run", ()).unwrap();
//     let _: () = loaded.method_call("org.kde.kwin.Script", "stop", ()).unwrap();
//     let _: () = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (&script_name,)).unwrap();
//     let _ = std::fs::remove_file(path);
// }

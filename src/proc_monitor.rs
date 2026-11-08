use crate::proc_utils::{get_proc_comm, get_proc_exe, get_proc_parent, matches_file_name};
use crate::program_tray::start_tray;
use crate::{ACTIVATED_GAME_MODE, GAME_OVERLAY_PIDS, GAME_PIDS, STEAM_CLIENT_PID, WEBHELPER_PID, continue_main, err, freeze_pid, log, on_game_exit, on_game_start};
use proc_connector::{EventMask, ProcConnector, ProcEvent};
use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::Ordering;

struct PidData {
    steam_root: u32, // no-steam-webhelper -> bash -> steam
    steam_client: u32, // steam -> srt-client-container -> srt-bwrap -> pv-adverb -> steam
    steam_launcher: u32, // steam -> srt-client-container -> x86_64-linux-gnu-srt-launcher-service
}

pub(crate) fn start_proc_connector() {
    let conn = ProcConnector::new().expect("failed to create process connector");
    if let Err(e) = conn.subscribe_filtered(EventMask::EXEC | EventMask::EXIT) {
        err!("Failed to set EventMask: {}", e);
    }
    let mut buf = vec![0u8; 4096];

    let mut pid_data = PidData { steam_root: 0, steam_client: 0, steam_launcher: 0 };

    std::thread::spawn(move || {
        if !capture_steam_proc(new_searching_set(), &mut pid_data, &conn, &mut buf) { return }
        start_tray();
        loop {
            capture_game_proc(&mut pid_data, &conn, &mut buf);
            // steam_client exited, maybe switching account
            if pid_data.steam_root == 0 {
                // the root process ended already, possible if it was killed by user
                continue_main();
                return;
            }
            // reset this, it exits later after steam_client exits, and restarts if switching account
            pid_data.steam_launcher = 0;

            let mut searching = new_searching_set();
            searching.insert(pid_data.steam_root);
            if !capture_steam_proc(searching, &mut pid_data, &conn, &mut buf) { return }
        }
    });
}

fn new_searching_set() -> HashSet<u32> {
    let mut searching: HashSet<u32> = HashSet::new();
    searching.insert(std::process::id());
    searching
}

fn capture_steam_proc(mut searching: HashSet<u32>, pid_data: &mut PidData, conn: &ProcConnector, buf: &mut [u8]) -> bool {
    log!("Capturing steam process...");
    loop {
        match conn.recv(buf) {
            Ok(event) => {
                match event {
                    ProcEvent::Exec { pid, .. } => {
                        if let Some(ppid) = get_proc_parent(pid) {
                            if searching.contains(&ppid) {
                                searching.insert(pid);
                                if let Some(comm) = get_proc_comm(pid) {
                                    match comm.as_str() {
                                        "x86_64-linux-gn" /* x86_64-linux-gnu-srt-launcher-service */ => {
                                            if pid_data.steam_launcher == 0 {
                                                pid_data.steam_launcher = pid;
                                                log!("steam_launcher pid is {pid}");
                                            }
                                        }
                                        "steam" => {
                                            if let Some(ppid) = get_proc_parent(pid) && get_proc_comm(ppid) == Some("pv-adverb".to_string()) {
                                                if pid_data.steam_client == 0 {
                                                    pid_data.steam_client = pid;
                                                    STEAM_CLIENT_PID.store(pid, Ordering::Relaxed);
                                                    log!("steam_client pid is {pid}");
                                                    return true;
                                                }
                                            } else if pid_data.steam_root == 0 && get_proc_exe(pid).map_or(false, |p| matches_file_name(&p, "steam")) {
                                                // Need to check proc_exe here otherwise will get bash (/bin/sh/usr/bin/steam)
                                                pid_data.steam_root = pid;
                                                log!("steam_root pid is {pid}");
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                    }
                    ProcEvent::Exit { pid, .. } => {
                        if searching.remove(&pid) && searching.len() == 1 {
                            // All steam processes exited, leaving this program pid only
                            log!("All steam processes exited");
                            continue_main();
                            return false;
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => { err!("error: {e:?}"); }
        }
    }
}

fn capture_game_proc(pid_data: &mut PidData, conn: &ProcConnector, buf: &mut [u8]) {
    log!("Capturing game process...");
    loop {
        match conn.recv(buf) {
            Ok(event) => {
                match event {
                    ProcEvent::Exec { pid, .. } => {
                        if let Some(ppid) = get_proc_parent(pid) {
                            if ppid == pid_data.steam_launcher {
                                if get_proc_comm(pid).map_or(false, |s| s.contains("steam-runtime")) {
                                    GAME_PIDS.lock().unwrap().push(pid);
                                    on_game_start();
                                    log!("Game pid {pid} started");
                                }
                            } else if ppid == pid_data.steam_client {
                                match get_proc_comm(pid).unwrap().as_str() {
                                    "steamwebhelper" => {
                                        if ACTIVATED_GAME_MODE.load(Ordering::Relaxed) {
                                            unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                                        } else {
                                            // We only store the root process of steam web helper
                                            // Term the root process kills all of them
                                            WEBHELPER_PID.store(pid, Ordering::Relaxed);
                                            log!("webhelper pid is {pid}");
                                        }
                                    }
                                    "gameoverlayui" => {
                                        GAME_OVERLAY_PIDS.lock().unwrap().push(pid);
                                        log!("gameoverlayui pid {pid} started");
                                        if ACTIVATED_GAME_MODE.load(Ordering::Relaxed) {
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
                        if parent_tgid == pid_data.steam_client {
                            if pid == WEBHELPER_PID.load(Ordering::Relaxed) {
                                WEBHELPER_PID.store(0, Ordering::Relaxed);
                                log!("webhelper pid {pid} exited");
                            } else if rm_elem(&GAME_OVERLAY_PIDS, pid) {
                                log!("gameoverlayui pid {pid} exited");
                            }
                        } else if parent_tgid == pid_data.steam_launcher {
                            if rm_elem(&GAME_PIDS, pid) {
                                log!("Game pid {pid} exited");
                                on_game_exit();
                            }
                        } else if pid == pid_data.steam_client {
                            pid_data.steam_client = 0;
                            log!("steam_client pid {pid} exited");
                            return;
                        } else if pid == pid_data.steam_root {
                            // well, user can send SIGTERM to steam_root, and the steam_client will still keep running
                            pid_data.steam_root = 0;
                            log!("steam_root pid {pid} exited");
                        }
                    }
                    _ => {}
                }
            }
            Err(e) => { err!("error: {e:?}"); }
        }
    }
}

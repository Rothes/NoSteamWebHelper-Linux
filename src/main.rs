use std::collections::HashMap;
use std::os::fd::{FromRawFd, OwnedFd};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Mutex;
use std::thread::sleep;
use std::time::Duration;
use dbus::blocking::Connection;
use sysinfo::{Pid, Process, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, RefreshKind, Signal, System, UpdateKind};

struct ContainerProcessInfo {
    steam_process: AtomicU32,
    launcher_process: AtomicU32,
}

static CONT_PROC: ContainerProcessInfo = ContainerProcessInfo { steam_process: AtomicU32::new(0), launcher_process: AtomicU32::new(0) };
static FROZEN_PROC: Mutex<Vec<u32>> = Mutex::new(Vec::new());
static EXISTS_GAME_EXIT_MONITOR: AtomicBool = AtomicBool::new(false);

fn main() {
    let mut child = Command::new("/usr/bin/steam").args(std::env::args().skip(1)).spawn().expect("Failed to launch steam");
    let steam_pid = child.id();
    {
        println!("[no-steam-webhelper] Launching steam steam pid {}", steam_pid);

        std::thread::spawn(move || {
            let _ = child.wait();
            _cleanup();
            std::process::exit(0);
        });
    }

    if let Err(err) = ctrlc::set_handler(move || {
        println!("[no-steam-webhelper] Caught exit signal");
        _cleanup();
        unsafe {
            let sys = create_sys();
            if check_steam_container_proc(sys.processes(), steam_pid) {
                libc::kill(CONT_PROC.steam_process.load(Ordering::Relaxed) as i32, libc::SIGTERM);
            }
        };
        std::process::exit(0);
    }) {
        eprintln!("[no-steam-webhelper] Failed to set ctrlc handler: {}", err);
    }

    monitor_game_proc_event(steam_pid);
    _cleanup();
    panic!("Huh?")
}

fn _cleanup() {
    let sys = create_sys();
    continue_frozen(&sys);
    println!("[no-steam-webhelper] Running cleanup");
}

fn continue_frozen(system: &System) {
    let mut list = FROZEN_PROC.lock().unwrap();
    list.iter().for_each(|pid| {
        system.process(Pid::from_u32(*pid)).map(|p| p.kill_with(Signal::Continue));
    });
    list.clear();
}

fn monitor_game_proc_event(steam_root: u32) {
    let conn = Connection::new_session().expect("Failed to connect to DBus");

    let _: () = conn.with_proxy(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        Duration::from_secs(5),
    ).method_call(
        "org.freedesktop.DBus.Monitoring",
        "BecomeMonitor",
        (vec!["interface='com.steampowered.PressureVessel.Launcher1'"], 0u32),
    ).expect("Failed to BecomeMonitor");

    // Skip the first two signal, NameAcquired for conn dbus and NameLost for BecomeMonitor
    for _ in 0..2 {
        conn.channel().blocking_pop_message(Duration::MAX).unwrap();
    }

    let steam_path = Some(&format!("{}/.local/share/Steam\0", std::env::var("HOME").unwrap()).into_bytes());

    while let Some(msg) = conn.channel().blocking_pop_message(Duration::MAX).unwrap() {
        if let Some(member) = msg.member() {
            match member.as_ref() {
                "Launch" => {
                    if msg.iter_init().get().as_ref() == steam_path {
                        // This calls even not playing a game,
                        // or calls and exits before actual game launches.
                        // Should ignore for game process launch detect.
                        continue
                    }

                    if EXISTS_GAME_EXIT_MONITOR.load(Ordering::Relaxed) {
                        continue
                    }

                    let mut sys = create_sys();
                    if !check_steam_container_proc(sys.processes(), steam_root) {
                        println!("[no-steam-webhelper] Bad steam container processes");
                        continue
                    }

                    let mut frozen_list = FROZEN_PROC.lock().unwrap();
                    if let Some(pid) = game_running(sys.processes()) {
                        start_monitor_game_exit(pid, steam_root);
                    } else {
                        continue
                    }

                    let swh = get_steam_webhelper_processes(&mut sys);

                    swh.first().map(|p| kwin_close_window(p.pid().as_u32()));
                    sleep(Duration::from_millis(50));

                    for proc in swh {
                        if proc.status() != ProcessStatus::Run {
                            continue
                        }
                        proc.kill_with(Signal::Stop);
                        frozen_list.push(proc.pid().as_u32());
                    }

                    println!("[no-steam-webhelper] freeze steamwebhelper");
                }
                // "ProcessExited" => {
                //     if !frozen {
                //         continue
                //     }
                //
                //     let sys = create_sys();
                //
                //     if !check_steam_container_proc(sys.processes(), steam_root) || has_game_running(sys.processes()) {
                //         continue
                //     }
                //
                //     continue_frozen(&sys);
                //     frozen = false;
                //     println!("[no-steam-webhelper] continue steamwebhelper");
                // }
                _ => {}
            }
        }
    }
}

fn kwin_close_window(pid: u32) {
    let script = format!(
        r#"
            const targetPid = {pid};
            const wins = workspace.windowList();

            for (const win of wins) {{
                if (win.pid === targetPid && win.closeable) {{
                    win.closeWindow();
                }}
            }}
        "#
    );
    let script_name = format!("no-steam-webhelper-close-script-tmp-{pid}");

    let path = std::env::temp_dir().join("no-steam-webhelper-close-script.tmp.js");
    std::fs::write(&path, script).unwrap();

    let conn = Connection::new_session().unwrap();
    let scripting = conn.with_proxy("org.kde.KWin", "/Scripting", Duration::from_secs(10));
    let script_id: i32;
    (script_id,) = scripting.method_call("org.kde.kwin.Scripting", "loadScript", (path.to_string_lossy(), &script_name)).unwrap_or((-555,));
    if script_id < 0 {
        eprintln!("[no-steam-webhelper] Failed to loadScript");
        return;
    }
    let loaded = conn.with_proxy(
        "org.kde.KWin",
        format!("/Scripting/Script{script_id}"),
        Duration::from_secs(10),
    );
    let _: () = loaded.method_call("org.kde.kwin.Script", "run", ()).unwrap();
    let _: () = loaded.method_call("org.kde.kwin.Script", "stop", ()).unwrap();
    let _: () = scripting.method_call("org.kde.kwin.Scripting", "unloadScript", (&script_name,)).unwrap();
    let _ = std::fs::remove_file(path);
}

fn start_monitor_game_exit(pid: u32, steam_root: u32) {
    if !EXISTS_GAME_EXIT_MONITOR.compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed).is_ok() {
        return;
    }
    std::thread::spawn(move || {
        let fd = unsafe {
            libc::syscall(
                libc::SYS_pidfd_open,
                pid as libc::pid_t,
                0,
            ) as i32
        };

        if fd >= 0 {
            let _pidfd = unsafe { OwnedFd::from_raw_fd(fd) };

            let mut pfd = libc::pollfd {
                fd,
                events: libc::POLLIN,
                revents: 0,
            };

            let ret = unsafe {
                libc::poll(&mut pfd, 1, -1)
            };

            if ret < 0 {
                eprintln!("[no-steam-webhelper] pollfd failed");
            }
        } else {
            eprintln!("[no-steam-webhelper] pidfd_open failed");
        }

        eprintln!("[no-steam-webhelper] Game process {pid} exited");
        EXISTS_GAME_EXIT_MONITOR.store(false, Ordering::Relaxed);

        let sys = create_sys();

        if !check_steam_container_proc(sys.processes(), steam_root) {
            return; // Exit?
        }

        if let Some(pid) = game_running(sys.processes()) {
            start_monitor_game_exit(pid, steam_root);
            return;
        }

        continue_frozen(&sys);
        println!("[no-steam-webhelper] continue steamwebhelper");
    });
}

fn create_sys() -> System {
    System::new_with_specifics(RefreshKind::nothing().with_processes(ProcessRefreshKind::nothing())).unwrap()
}

fn check_steam_container_proc(proc: &HashMap<Pid, Process>, _steam_root: u32) -> bool {
    macro_rules! find_children {
        ($pid:expr) => {
            proc.values().filter(|process| process.parent().as_ref() == Some($pid))
        };
    }
    if !check_proc_running(proc, &CONT_PROC.steam_process) || !check_proc_running(proc, &CONT_PROC.launcher_process) {
        let mut ok: u8 = 0;

        if let Some(container_entry) = proc.iter().find(|(_, process)| {
            process.name() == "srt-client-cont" /* srt-client-container */ && if let Some(ppid) = process.parent() {
                proc.get(&ppid).map_or(false, |p| p.name() == "steam")
            } else { false }
        }) {
            for p in find_children!(container_entry.0) {
                match p.name().to_str().unwrap() {
                    "x86_64-linux-gn" /* x86_64-linux-gnu-srt-launcher-service */ => {
                        CONT_PROC.launcher_process.store(p.pid().as_u32(), Ordering::Relaxed);
                        ok += 1;
                    }
                    "srt-bwrap" => {
                        for p in find_children!(&p.pid()) {
                            if p.name() == "pv-adverb" {
                                for p in find_children!(&p.pid()) {
                                    if p.name() == "steam" {
                                        CONT_PROC.steam_process.store(p.pid().as_u32(), Ordering::Relaxed);
                                        ok += 1;
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if ok == 2 {
            println!("[no-steam-webhelper] webhelper parent pid: {}, launcher pid: {}", CONT_PROC.steam_process.load(Ordering::Relaxed), CONT_PROC.launcher_process.load(Ordering::Relaxed));
        }
        ok == 2
    } else {
        true
    }
}

fn check_proc_running(proc: &HashMap<Pid, Process>, pid: &AtomicU32) -> bool {
    proc.contains_key(&Pid::from_u32(pid.load(Ordering::Relaxed)))
}

fn game_running(proc: &HashMap<Pid, Process>) -> Option<u32> {
    let ppid_expected = CONT_PROC.launcher_process.load(Ordering::Relaxed);
    let find = proc.values().find(|p| p.parent().map_or(false, |pp| pp.as_u32() == ppid_expected) && p.name().to_str().unwrap().contains("steam-runtime"));

    if let Some(proc) = find {
        println!("[no-steam-webhelper] Currently running game: {:?}", proc);
        Some(proc.pid().as_u32())
    } else {
        None
    }
}

fn get_steam_webhelper_processes(sys: &mut System) -> Vec<&Process> {
    let ppid_expected = CONT_PROC.steam_process.load(Ordering::Relaxed);
    let mut possible_pids = Vec::new();
    let mut ppid = vec![ppid_expected];

    while let Some(parent) = ppid.pop() {
        for (pid, p) in sys.processes() {
            if p.parent().map_or(false, |pp| pp.as_u32() == parent) {
                possible_pids.push(*pid);
                ppid.push(pid.as_u32());
            }
        }
    }

    sys.refresh_processes_specifics(ProcessesToUpdate::Some(&possible_pids), false, ProcessRefreshKind::nothing().with_exe(UpdateKind::Always));

    let mut webhelper_proc = Vec::new();
    for pid in possible_pids {
        if let Some(p) = sys.process(pid) {
            let is_webhelper = p.exe()
                .and_then(|e| e.file_name())
                .and_then(|s| s.to_str())
                .map_or(false, |name| name == "steamwebhelper");

            if is_webhelper {
                webhelper_proc.push(p);
            }
        }
    }

    webhelper_proc
}
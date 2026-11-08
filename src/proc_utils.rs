use std::path::PathBuf;

pub fn get_proc_parent(pid: u32) -> Option<u32> {
    let path = format!("/proc/{}/stat", pid);
    let content = std::fs::read_to_string(path).ok()?;
    let ppid_start = content.rfind(')')? + 4;
    let ppid = &content[ppid_start..];
    let ppid_end = ppid.find(' ')?;
    ppid[..ppid_end].parse().ok()
}

pub fn get_proc_exe(pid: u32) -> Option<PathBuf> {
    let path = format!("/proc/{}/exe", pid);
    std::fs::read_link(path).ok()
}

// pub fn get_proc_cmdline(pid: u32) -> Option<String> {
//     let path = format!("/proc/{}/cmdline", pid);
//     std::fs::read_to_string(path).ok()
// }

pub fn get_proc_comm(pid: u32) -> Option<String> {
    let path = format!("/proc/{}/comm", pid);
    // why calling trim_end: remove newline at the end
    std::fs::read_to_string(path).map(|s| s.trim_end().to_string()).ok()
}

pub fn matches_file_name(path: &PathBuf, name: &str) -> bool {
    path.file_name().and_then(|n| n.to_str()).map_or(false, |n| n == name)
}

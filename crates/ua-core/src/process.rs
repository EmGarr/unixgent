//! Process introspection: depth counting and CWD resolution.
//!
//! Depth counting: Instead of trusting an environment variable (which the LLM
//! could reset), we walk the real process tree maintained by the kernel. Count
//! how many ancestor processes are the same binary as us — that's our depth.
//!
//! CWD resolution: Query the working directory of a child process via OS APIs
//! (proc_pidinfo on macOS, /proc on Linux). Used to track the PTY child shell's
//! current directory instead of the parent process's stale CWD.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Count how many ancestors of `my_pid` have an executable matching `my_exe`.
///
/// `info_fn(pid)` returns `(parent_pid, exe_path)` for the given process,
/// or `None` if the process doesn't exist / can't be inspected.
///
/// This is the testable core — tests provide a fake `info_fn`, production
/// provides the real OS-level one.
fn count_matching_ancestors(
    my_exe: &Path,
    my_pid: u32,
    info_fn: &dyn Fn(u32) -> Option<(u32, PathBuf)>,
) -> u32 {
    let mut depth = 0;
    let mut seen = HashSet::new();
    seen.insert(my_pid);

    // Get our parent
    let mut current = match info_fn(my_pid) {
        Some((ppid, _)) => ppid,
        None => return 0,
    };

    while current > 1 && seen.insert(current) {
        if let Some((next_ppid, exe)) = info_fn(current) {
            if exe == my_exe {
                depth += 1;
            }
            current = next_ppid;
        } else {
            break;
        }
    }

    depth
}

/// Count how many ancestor processes are the same binary as the current process.
///
/// Returns 0 if we can't determine our exe or can't walk the tree (e.g. on
/// unsupported platforms). This is the safe default — it means "no nesting
/// detected, allow execution."
pub fn count_ancestor_depth() -> u32 {
    let my_exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(_) => return 0,
    };
    count_matching_ancestors(&my_exe, std::process::id(), &platform::process_info)
}

/// Check depth against the limit. Returns `Ok(depth)` if under the limit,
/// `Err(depth)` if at or over.
pub fn check_depth(max: u32) -> Result<u32, u32> {
    let depth = count_ancestor_depth();
    if depth >= max {
        Err(depth)
    } else {
        Ok(depth)
    }
}

/// Query the current working directory of a process by PID.
///
/// Uses OS-specific APIs:
/// - **macOS**: `proc_pidinfo` with `PROC_PIDVNODEPATHINFO`
/// - **Linux**: `readlink /proc/{pid}/cwd`
/// - **Other**: returns `None`
pub fn cwd_of_pid(pid: u32) -> Option<String> {
    platform::cwd_of(pid)
}

/// Collect PIDs of descendant processes of `ancestor_pid` that are running
/// the same binary as `my_exe`.
///
/// This is the testable core — tests provide a fake `info_fn`, production
/// provides the real OS-level one.
fn collect_descendant_agents_core(
    my_exe: &Path,
    ancestor_pid: u32,
    all_pids: &[u32],
    info_fn: &dyn Fn(u32) -> Option<(u32, PathBuf)>,
) -> Vec<u32> {
    let mut result = Vec::new();
    for &pid in all_pids {
        if pid == ancestor_pid {
            continue;
        }
        // Quick filter: only consider processes with the same executable
        let (ppid, exe) = match info_fn(pid) {
            Some(info) => info,
            None => continue,
        };
        if exe != my_exe {
            continue;
        }
        // Walk the parent chain to see if ancestor_pid is an ancestor
        let mut current = ppid;
        let mut seen = HashSet::new();
        seen.insert(pid);
        while current > 1 && seen.insert(current) {
            if current == ancestor_pid {
                result.push(pid);
                break;
            }
            match info_fn(current) {
                Some((next_ppid, _)) => current = next_ppid,
                None => break,
            }
        }
    }
    result
}

/// Count how many descendant agent processes are running under `ancestor_pid`.
///
/// Returns 0 if we can't determine our exe or enumerate processes.
pub fn count_descendant_agents(ancestor_pid: u32) -> u32 {
    list_descendant_agent_pids(ancestor_pid).len() as u32
}

/// List PIDs of descendant agent processes running under `ancestor_pid`.
///
/// Returns an empty vec if we can't determine our exe or enumerate processes.
pub fn list_descendant_agent_pids(ancestor_pid: u32) -> Vec<u32> {
    let my_exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let all_pids = platform::list_all_pids();
    collect_descendant_agents_core(&my_exe, ancestor_pid, &all_pids, &platform::process_info)
}

// --- Platform-specific process info ---

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    const PROC_PIDTBSDINFO: libc::c_int = 3;
    const PROC_PIDVNODEPATHINFO: libc::c_int = 9;

    /// Minimal repr(C) layout of `struct proc_bsdinfo` from <sys/proc_info.h>.
    /// We only read `pbi_ppid`; trailing fields are opaque padding so the
    /// total size (136 bytes) matches what `proc_pidinfo` expects.
    #[repr(C)]
    struct ProcBsdInfo {
        pbi_flags: u32,
        pbi_status: u32,
        pbi_xstatus: u32,
        pbi_pid: u32,
        pbi_ppid: u32,
        _rest: [u8; 136 - 20],
    }

    /// Layout of `struct vnode_info_path` (152 bytes).
    /// We only need `vip_path` — the rest is opaque padding.
    #[repr(C)]
    struct VnodeInfoPath {
        _vnode_info: [u8; 152], // struct vnode_info (opaque)
        vip_path: [u8; 1024],   // MAXPATHLEN
    }

    /// Layout of `struct proc_vnodepathinfo`.
    /// Contains two VnodeInfoPath structs: cdir (current dir) and rdir (root dir).
    #[repr(C)]
    struct ProcVnodePathInfo {
        pvi_cdir: VnodeInfoPath,
        _pvi_rdir: VnodeInfoPath,
    }

    extern "C" {
        fn proc_pidpath(
            pid: libc::c_int,
            buffer: *mut libc::c_void,
            buffersize: u32,
        ) -> libc::c_int;
        fn proc_pidinfo(
            pid: libc::c_int,
            flavor: libc::c_int,
            arg: u64,
            buffer: *mut libc::c_void,
            buffersize: libc::c_int,
        ) -> libc::c_int;
        fn proc_listallpids(buffer: *mut libc::c_void, buffersize: libc::c_int) -> libc::c_int;
    }

    pub fn process_info(pid: u32) -> Option<(u32, PathBuf)> {
        let ppid = ppid_of(pid)?;
        let exe = exe_of(pid)?;
        Some((ppid, exe))
    }

    pub fn cwd_of(pid: u32) -> Option<String> {
        let mut info: ProcVnodePathInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ProcVnodePathInfo>() as libc::c_int;
        let ret = unsafe {
            proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDVNODEPATHINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if ret == size {
            let path_bytes = &info.pvi_cdir.vip_path;
            let nul_pos = path_bytes
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(path_bytes.len());
            let path = std::str::from_utf8(&path_bytes[..nul_pos]).ok()?;
            if path.is_empty() {
                None
            } else {
                Some(path.to_string())
            }
        } else {
            None
        }
    }

    fn exe_of(pid: u32) -> Option<PathBuf> {
        let mut buf = [0u8; 4096];
        let ret = unsafe {
            proc_pidpath(
                pid as libc::c_int,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len() as u32,
            )
        };
        if ret > 0 {
            let path = std::str::from_utf8(&buf[..ret as usize]).ok()?;
            let p = PathBuf::from(path);
            Some(p.canonicalize().unwrap_or(p))
        } else {
            None
        }
    }

    fn ppid_of(pid: u32) -> Option<u32> {
        let mut info: ProcBsdInfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<ProcBsdInfo>() as libc::c_int;
        let ret = unsafe {
            proc_pidinfo(
                pid as libc::c_int,
                PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            )
        };
        if ret == size {
            Some(info.pbi_ppid)
        } else {
            None
        }
    }

    pub fn list_all_pids() -> Vec<u32> {
        // First call with null buffer to get the count
        let count = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
        if count <= 0 {
            return Vec::new();
        }
        // Allocate with some extra room for new processes
        let capacity = (count as usize) + 64;
        let mut buf: Vec<libc::c_int> = vec![0; capacity];
        let buf_size = (capacity * std::mem::size_of::<libc::c_int>()) as libc::c_int;
        let actual = unsafe { proc_listallpids(buf.as_mut_ptr() as *mut libc::c_void, buf_size) };
        if actual <= 0 {
            return Vec::new();
        }
        buf.truncate(actual as usize);
        buf.into_iter()
            .filter(|&p| p > 0)
            .map(|p| p as u32)
            .collect()
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    pub fn process_info(pid: u32) -> Option<(u32, PathBuf)> {
        let ppid = ppid_of(pid)?;
        let exe = exe_of(pid)?;
        Some((ppid, exe))
    }

    pub fn cwd_of(pid: u32) -> Option<String> {
        let link = std::fs::read_link(format!("/proc/{pid}/cwd")).ok()?;
        Some(link.to_string_lossy().to_string())
    }

    fn exe_of(pid: u32) -> Option<PathBuf> {
        let link = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        Some(link.canonicalize().unwrap_or(link))
    }

    fn ppid_of(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // Format: "pid (comm) state ppid ..."
        // comm can contain spaces and parens, so find the last ')'
        let after_comm = stat.rfind(')')? + 2;
        let fields: Vec<&str> = stat[after_comm..].split_whitespace().collect();
        // fields[0] = state, fields[1] = ppid
        fields.get(1)?.parse().ok()
    }

    pub fn list_all_pids() -> Vec<u32> {
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
            .collect()
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    pub fn process_info(_pid: u32) -> Option<(u32, PathBuf)> {
        None // Can't introspect process tree on this platform
    }

    pub fn cwd_of(_pid: u32) -> Option<String> {
        None // Can't introspect process CWD on this platform
    }

    pub fn list_all_pids() -> Vec<u32> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// Build a fake info_fn from a process table.
    fn fake_info(
        table: &HashMap<u32, (u32, PathBuf)>,
    ) -> impl Fn(u32) -> Option<(u32, PathBuf)> + '_ {
        move |pid| table.get(&pid).cloned()
    }

    #[test]
    fn depth_zero_no_ancestors() {
        // Single unixagent, parent is init
        let table: HashMap<u32, (u32, PathBuf)> =
            [(100, (1, PathBuf::from("/usr/bin/unixagent")))].into();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 100, &fake_info(&table));
        assert_eq!(depth, 0);
    }

    #[test]
    fn depth_one_direct_parent() {
        // unixagent(100) -> sh(101) -> unixagent(102)
        let table: HashMap<u32, (u32, PathBuf)> = [
            (102, (101, PathBuf::from("/usr/bin/unixagent"))),
            (101, (100, PathBuf::from("/bin/sh"))),
            (100, (1, PathBuf::from("/usr/bin/unixagent"))),
        ]
        .into();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 102, &fake_info(&table));
        assert_eq!(depth, 1);
    }

    #[test]
    fn depth_two_with_intermediate_shells() {
        // unixagent(100) -> sh(101) -> unixagent(102) -> sh(103) -> unixagent(104)
        let table: HashMap<u32, (u32, PathBuf)> = [
            (104, (103, PathBuf::from("/usr/bin/unixagent"))),
            (103, (102, PathBuf::from("/bin/sh"))),
            (102, (101, PathBuf::from("/usr/bin/unixagent"))),
            (101, (100, PathBuf::from("/bin/sh"))),
            (100, (1, PathBuf::from("/usr/bin/unixagent"))),
        ]
        .into();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 104, &fake_info(&table));
        assert_eq!(depth, 2);
    }

    #[test]
    fn skips_non_matching_ancestors() {
        // bash(50) -> vim(60) -> sh(70) -> unixagent(80) -> sh(90) -> unixagent(100)
        let table: HashMap<u32, (u32, PathBuf)> = [
            (100, (90, PathBuf::from("/usr/bin/unixagent"))),
            (90, (80, PathBuf::from("/bin/sh"))),
            (80, (70, PathBuf::from("/usr/bin/unixagent"))),
            (70, (60, PathBuf::from("/bin/sh"))),
            (60, (50, PathBuf::from("/usr/bin/vim"))),
            (50, (1, PathBuf::from("/bin/bash"))),
        ]
        .into();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 100, &fake_info(&table));
        // Only pid 80 is a matching ancestor (not 50, 60, 70, 90)
        assert_eq!(depth, 1);
    }

    #[test]
    fn handles_missing_process() {
        // Parent doesn't exist in the table
        let table: HashMap<u32, (u32, PathBuf)> =
            [(100, (999, PathBuf::from("/usr/bin/unixagent")))].into();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 100, &fake_info(&table));
        assert_eq!(depth, 0);
    }

    #[test]
    fn handles_self_not_in_table() {
        let table: HashMap<u32, (u32, PathBuf)> = HashMap::new();

        let depth =
            count_matching_ancestors(Path::new("/usr/bin/unixagent"), 100, &fake_info(&table));
        assert_eq!(depth, 0);
    }

    #[test]
    fn different_binary_path_no_match() {
        // cargo-built binary vs installed binary — different paths, no match
        let table: HashMap<u32, (u32, PathBuf)> = [
            (102, (101, PathBuf::from("/tmp/target/debug/unixagent"))),
            (101, (100, PathBuf::from("/bin/sh"))),
            (100, (1, PathBuf::from("/usr/local/bin/unixagent"))),
        ]
        .into();

        let depth = count_matching_ancestors(
            Path::new("/tmp/target/debug/unixagent"),
            102,
            &fake_info(&table),
        );
        // Parent is a different binary path, so depth = 0
        assert_eq!(depth, 0);
    }

    #[test]
    fn same_cargo_binary_matches() {
        // Both are the same cargo-built binary
        let exe = PathBuf::from("/home/user/project/target/debug/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (102, (101, exe.clone())),
            (101, (100, PathBuf::from("/bin/sh"))),
            (100, (1, exe.clone())),
        ]
        .into();

        let depth = count_matching_ancestors(&exe, 102, &fake_info(&table));
        assert_eq!(depth, 1);
    }

    #[test]
    fn check_depth_under_limit() {
        // This tests the real process — we're not nested, so depth should be 0
        let result = check_depth(3);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[test]
    fn platform_can_read_current_process() {
        // Verify platform::process_info works for our own process
        let info = platform::process_info(std::process::id());
        assert!(info.is_some(), "should be able to read own process info");
        let (ppid, exe) = info.unwrap();
        assert!(ppid > 0, "ppid should be > 0");
        assert!(exe.exists(), "exe path should exist: {}", exe.display());
    }

    #[test]
    fn platform_returns_none_for_nonexistent_pid() {
        // PID 999999999 almost certainly doesn't exist
        let info = platform::process_info(999_999_999);
        assert!(info.is_none());
    }

    #[test]
    fn cwd_of_pid_returns_current_process_cwd() {
        let cwd = cwd_of_pid(std::process::id());
        assert!(cwd.is_some(), "should be able to read own CWD");
        let cwd = cwd.unwrap();
        assert!(!cwd.is_empty(), "CWD should not be empty");
        // Should match std::env::current_dir
        let expected = std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .to_string();
        assert_eq!(cwd, expected);
    }

    #[test]
    fn cwd_of_pid_returns_none_for_nonexistent() {
        let cwd = cwd_of_pid(999_999_999);
        assert!(cwd.is_none());
    }

    // --- Descendant collection tests ---

    #[test]
    fn descendants_none() {
        // No other processes → empty
        let table: HashMap<u32, (u32, PathBuf)> =
            [(100, (1, PathBuf::from("/usr/bin/unixagent")))].into();
        let pids = collect_descendant_agents_core(
            Path::new("/usr/bin/unixagent"),
            100,
            &[100],
            &fake_info(&table),
        );
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_direct_child() {
        // ancestor(100) → sh(101) → ua(102)
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (100, (1, ua.clone())),
            (101, (100, PathBuf::from("/bin/sh"))),
            (102, (101, ua.clone())),
        ]
        .into();
        let pids = collect_descendant_agents_core(&ua, 100, &[100, 101, 102], &fake_info(&table));
        assert_eq!(pids, vec![102]);
    }

    #[test]
    fn descendants_nested() {
        // ancestor(100) → sh(101) → ua(102) → sh(103) → ua(104)
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (100, (1, ua.clone())),
            (101, (100, PathBuf::from("/bin/sh"))),
            (102, (101, ua.clone())),
            (103, (102, PathBuf::from("/bin/sh"))),
            (104, (103, ua.clone())),
        ]
        .into();
        let pids = collect_descendant_agents_core(
            &ua,
            100,
            &[100, 101, 102, 103, 104],
            &fake_info(&table),
        );
        assert_eq!(pids.len(), 2);
        assert!(pids.contains(&102));
        assert!(pids.contains(&104));
    }

    #[test]
    fn descendants_non_matching_exe() {
        // ancestor(100) → sh(101) → python(102) — different exe
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (100, (1, ua.clone())),
            (101, (100, PathBuf::from("/bin/sh"))),
            (102, (101, PathBuf::from("/usr/bin/python"))),
        ]
        .into();
        let pids = collect_descendant_agents_core(&ua, 100, &[100, 101, 102], &fake_info(&table));
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_unrelated_agent() {
        // ancestor(100), unrelated ua(200) with parent 1 — not a descendant
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> =
            [(100, (1, ua.clone())), (200, (1, ua.clone()))].into();
        let pids = collect_descendant_agents_core(&ua, 100, &[100, 200], &fake_info(&table));
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_ancestor_not_counted() {
        // ancestor(100) itself should never be counted
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [(100, (1, ua.clone()))].into();
        let pids = collect_descendant_agents_core(&ua, 100, &[100], &fake_info(&table));
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_empty_pid_list() {
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = HashMap::new();
        let pids = collect_descendant_agents_core(&ua, 100, &[], &fake_info(&table));
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_missing_ancestor() {
        // ancestor_pid not in the table — descendants can't find it
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (102, (101, ua.clone())),
            (101, (999, PathBuf::from("/bin/sh"))),
        ]
        .into();
        let pids = collect_descendant_agents_core(&ua, 100, &[101, 102], &fake_info(&table));
        assert!(pids.is_empty());
    }

    #[test]
    fn descendants_mixed() {
        // ancestor(100) → sh(101) → ua(102) → sh(103) → python(104)
        //                         → sh(105) → ua(106)
        let ua = PathBuf::from("/usr/bin/unixagent");
        let table: HashMap<u32, (u32, PathBuf)> = [
            (100, (1, ua.clone())),
            (101, (100, PathBuf::from("/bin/sh"))),
            (102, (101, ua.clone())),
            (103, (102, PathBuf::from("/bin/sh"))),
            (104, (103, PathBuf::from("/usr/bin/python"))),
            (105, (100, PathBuf::from("/bin/sh"))),
            (106, (105, ua.clone())),
        ]
        .into();
        let pids = collect_descendant_agents_core(
            &ua,
            100,
            &[100, 101, 102, 103, 104, 105, 106],
            &fake_info(&table),
        );
        // 102 and 106 are descendant agents
        assert_eq!(pids.len(), 2);
        assert!(pids.contains(&102));
        assert!(pids.contains(&106));
    }

    #[test]
    fn list_all_pids_includes_self() {
        let pids = platform::list_all_pids();
        let my_pid = std::process::id();
        assert!(
            pids.contains(&my_pid),
            "list_all_pids should include our own PID ({my_pid})"
        );
    }
}

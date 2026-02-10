//! Process-tree depth counting for batch-mode recursion control.
//!
//! Instead of trusting an environment variable (which the LLM could reset),
//! we walk the real process tree maintained by the kernel. Count how many
//! ancestor processes are the same binary as us — that's our depth.

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

// --- Platform-specific process info ---

#[cfg(target_os = "macos")]
mod platform {
    use std::path::PathBuf;

    const PROC_PIDTBSDINFO: libc::c_int = 3;

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
    }

    pub fn process_info(pid: u32) -> Option<(u32, PathBuf)> {
        let ppid = ppid_of(pid)?;
        let exe = exe_of(pid)?;
        Some((ppid, exe))
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
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    pub fn process_info(pid: u32) -> Option<(u32, PathBuf)> {
        let ppid = ppid_of(pid)?;
        let exe = exe_of(pid)?;
        Some((ppid, exe))
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
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    pub fn process_info(_pid: u32) -> Option<(u32, PathBuf)> {
        None // Can't introspect process tree on this platform
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
}

//! Linux Landlock implementation.
//!
//! Uses the `landlock` crate with ABI V5 and BestEffort compatibility.
//! Default-deny: only paths listed in the policy are accessible.

use crate::policy::SandboxPolicy;
use crate::SandboxError;

use landlock::{
    Access, AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr, RulesetStatus,
    ABI,
};

/// Apply Landlock filesystem sandbox to the current process. Irreversible.
pub fn apply_landlock(policy: &SandboxPolicy) -> Result<(), SandboxError> {
    let abi = ABI::V5;

    let mut ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .map_err(|e| SandboxError::Platform(format!("Landlock ruleset creation failed: {e}")))?
        .create()
        .map_err(|e| SandboxError::Platform(format!("Landlock ruleset create failed: {e}")))?;

    let read_access = AccessFs::from_read(abi);
    let all_access = AccessFs::from_all(abi);

    // Writable paths get full access
    for path in &policy.writable {
        if let Ok(fd) = PathFd::new(path) {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, all_access))
                .map_err(|e| {
                    SandboxError::Platform(format!(
                        "Landlock add writable rule for {}: {e}",
                        path.display()
                    ))
                })?;
        }
        // Skip paths that don't exist — they can't be accessed anyway
    }

    // Readable paths get read-only access
    for path in &policy.readable {
        if let Ok(fd) = PathFd::new(path) {
            ruleset = ruleset
                .add_rule(PathBeneath::new(fd, read_access))
                .map_err(|e| {
                    SandboxError::Platform(format!(
                        "Landlock add readable rule for {}: {e}",
                        path.display()
                    ))
                })?;
        }
    }

    // Denied paths are enforced by omission — Landlock is default-deny,
    // so anything not explicitly allowed is blocked. We don't need to
    // add deny rules; we simply don't add allow rules for denied paths.
    //
    // Note: if a denied path is a subdirectory of an allowed writable path,
    // Landlock cannot enforce the deny. This is a known limitation — the
    // policy should be constructed so denied paths are not under writable paths.

    let status = ruleset
        .restrict_self()
        .map_err(|e| SandboxError::Platform(format!("Landlock restrict_self failed: {e}")))?;

    match status.ruleset {
        RulesetStatus::FullyEnforced => Ok(()),
        RulesetStatus::PartiallyEnforced => {
            eprintln!("[ua:sandbox] warning: Landlock partially enforced (kernel may lack full ABI support)");
            Ok(())
        }
        RulesetStatus::NotEnforced => Err(SandboxError::Platform(
            "Landlock not enforced (kernel support missing?)".to_string(),
        )),
    }
}

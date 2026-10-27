//! Skill freshness: compare each installed Claude skill against the copy this
//! binary ships. A repo checkout under cwd is not consulted: an older checkout
//! would report a current install as stale and suggest downgrading it.

use crate::commands::admin::claude::{
    claude_home, claude_skill_dir, embedded_skill_dir, inject_ecp_import_at, install_skill_from,
    ClaudeSkillTarget,
};
use crate::commands::admin::doctor::CheckResult;
use crate::commands::admin::skill_fs::{
    install_state, skill_diff, write_install_record, InstallState,
};
use std::path::Path;

pub(crate) fn check(fix: bool) -> Vec<CheckResult> {
    let home = claude_home();
    ClaudeSkillTarget::All
        .expand()
        .iter()
        .map(|&skill| match embedded_skill_dir(skill) {
            Ok(src) => check_at(skill, src.path(), &claude_skill_dir(skill), &home, fix),
            Err(e) => CheckResult::fail(
                format!("skill:{}", skill.name()),
                format!("cannot resolve source: {e}"),
            ),
        })
        .collect()
}

/// `--fix` installs only a skill that is missing, or one whose files still
/// match what ecp installed: overwriting an edited skill would destroy work.
fn check_at(
    skill: ClaudeSkillTarget,
    src: &Path,
    dst: &Path,
    claude_home: &Path,
    fix: bool,
) -> CheckResult {
    let name = format!("skill:{}", skill.name());
    let install = format!("ecp admin claude install skills {}", skill.name());
    let review = format!("review with `{install} --dry-run`, then run it without --dry-run");

    // A dir that exists without SKILL.md is still the user's files: only an
    // absent or empty dir counts as not installed.
    let absent = std::fs::read_dir(dst).map_or(true, |mut d| d.next().is_none());
    let mut result = if absent {
        CheckResult::warn(&name, "not installed").with_remediation(&install)
    } else {
        let diff = match skill_diff(src, dst, true) {
            Ok(d) => d,
            Err(e) => return CheckResult::fail(name, format!("diff failed: {e}")),
        };
        let state = match install_state(dst) {
            Ok(s) => s,
            Err(e) => return CheckResult::fail(name, format!("read install record: {e}")),
        };
        if !diff.has_changes() {
            // Recording an identical install lets a later --fix update it.
            if fix && state != InstallState::Untouched {
                if let Err(e) = write_install_record(dst, src) {
                    return CheckResult::warn(
                        name,
                        format!("up to date; writing the install record failed: {e}"),
                    );
                }
            }
            return CheckResult::ok(name, "up to date");
        }
        match state {
            InstallState::Untouched => CheckResult::warn(
                &name,
                "outdated — differs from this ecp's copy, no local edits",
            )
            .with_remediation(&install),
            InstallState::Edited(n) => {
                return CheckResult::warn(
                    name,
                    format!(
                        "locally edited — {n} file(s) changed since ecp installed it; \
                         installing overwrites them"
                    ),
                )
                .with_remediation(review)
            }
            InstallState::Unrecorded => {
                return CheckResult::warn(
                    name,
                    "differs from this ecp's copy; no install record, so local edits \
                     cannot be ruled out",
                )
                .with_remediation(review)
            }
        }
    };

    if fix {
        let installed = install_skill_from(skill, src, dst, false).and_then(|()| {
            if matches!(skill, ClaudeSkillTarget::Ecp) {
                inject_ecp_import_at(claude_home, &src.join("ECP.md"), false)?;
            }
            Ok(())
        });
        result.fix_applied = Some(installed.is_ok());
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::admin::doctor::CheckStatus;
    use std::fs;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, content).unwrap();
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        src: std::path::PathBuf,
        dst: std::path::PathBuf,
        home: std::path::PathBuf,
    }

    /// `dst` holds an install of the old source, with its install record.
    fn installed_then_upgraded() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let dst = tmp.path().join("home/skills/simplify");
        write(&src, "SKILL.md", "old\n");
        write(&src, "helper.sh", "echo old\n");
        install_skill_from(ClaudeSkillTarget::Simplify, &src, &dst, false).unwrap();
        write(&src, "SKILL.md", "new\n");
        Fixture {
            src,
            dst,
            home: tmp.path().join("home"),
            _tmp: tmp,
        }
    }

    fn run(f: &Fixture, fix: bool) -> CheckResult {
        check_at(ClaudeSkillTarget::Simplify, &f.src, &f.dst, &f.home, fix)
    }

    #[test]
    fn test_check_at_untouched_old_install_fix_updates_it() {
        let f = installed_then_upgraded();
        let r = run(&f, true);
        assert_eq!(r.status, CheckStatus::Warn);
        assert!(r.message.starts_with("outdated"), "{}", r.message);
        assert_eq!(r.fix_applied, Some(true));
        assert_eq!(fs::read_to_string(f.dst.join("SKILL.md")).unwrap(), "new\n");
        assert_eq!(install_state(&f.dst).unwrap(), InstallState::Untouched);
    }

    #[test]
    fn test_check_at_edited_install_fix_keeps_edits() {
        let f = installed_then_upgraded();
        write(&f.dst, "helper.sh", "echo mine\n");
        write(&f.dst, "extra.md", "my notes\n");
        let r = run(&f, true);
        assert!(
            r.message.starts_with("locally edited — 2 file(s)"),
            "{}",
            r.message
        );
        assert!(r.remediation.unwrap().contains("--dry-run"));
        assert_eq!(r.fix_applied, None);
        assert_eq!(fs::read_to_string(f.dst.join("SKILL.md")).unwrap(), "old\n");
        assert!(f.dst.join("extra.md").is_file());
    }

    #[test]
    fn test_check_at_unrecorded_install_fix_leaves_it() {
        let f = installed_then_upgraded();
        fs::remove_file(f.dst.join(".ecp-install.json")).unwrap();
        let r = run(&f, true);
        assert!(r.message.contains("no install record"), "{}", r.message);
        assert_eq!(r.fix_applied, None);
        assert_eq!(fs::read_to_string(f.dst.join("SKILL.md")).unwrap(), "old\n");
    }

    #[test]
    fn test_check_at_unrecorded_up_to_date_fix_records_it() {
        let f = installed_then_upgraded();
        write(&f.src, "SKILL.md", "old\n");
        fs::remove_file(f.dst.join(".ecp-install.json")).unwrap();
        let r = run(&f, true);
        assert_eq!(r.status, CheckStatus::Ok);
        assert_eq!(install_state(&f.dst).unwrap(), InstallState::Untouched);
    }

    #[test]
    fn test_check_at_edited_back_to_source_fix_rerecords_it() {
        let f = installed_then_upgraded();
        write(&f.dst, "SKILL.md", "new\n");
        assert_eq!(install_state(&f.dst).unwrap(), InstallState::Edited(1));
        let r = run(&f, true);
        assert_eq!(r.status, CheckStatus::Ok);
        assert_eq!(install_state(&f.dst).unwrap(), InstallState::Untouched);
    }

    #[test]
    fn test_check_at_skill_md_deleted_fix_keeps_remaining_files() {
        let f = installed_then_upgraded();
        write(&f.dst, "helper.sh", "echo mine\n");
        fs::remove_file(f.dst.join("SKILL.md")).unwrap();
        let r = run(&f, true);
        assert!(r.message.starts_with("locally edited"), "{}", r.message);
        assert_eq!(r.fix_applied, None);
        assert_eq!(
            fs::read_to_string(f.dst.join("helper.sh")).unwrap(),
            "echo mine\n"
        );
    }

    #[test]
    fn test_check_at_not_installed_fix_installs() {
        let f = installed_then_upgraded();
        fs::remove_dir_all(&f.dst).unwrap();
        let r = run(&f, true);
        assert_eq!(r.message, "not installed");
        assert_eq!(r.fix_applied, Some(true));
        assert_eq!(fs::read_to_string(f.dst.join("SKILL.md")).unwrap(), "new\n");
    }

    #[test]
    fn test_check_at_without_fix_changes_nothing() {
        let f = installed_then_upgraded();
        let r = run(&f, false);
        assert!(r.message.starts_with("outdated"), "{}", r.message);
        assert_eq!(r.fix_applied, None);
        assert_eq!(fs::read_to_string(f.dst.join("SKILL.md")).unwrap(), "old\n");
    }
}

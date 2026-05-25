//! Skill freshness: compare each installed Claude skill against its repo source.

use crate::commands::admin::claude::{
    claude_skill_dir, install_skills, source_skill_dir_at, ClaudeSkillTarget,
};
use crate::commands::admin::doctor::CheckResult;
use crate::commands::admin::skill_fs::skill_diff;

pub(crate) fn check(fix: bool) -> Vec<CheckResult> {
    let cwd = match std::env::current_dir() {
        Ok(c) => c,
        Err(e) => return vec![CheckResult::fail("skills", format!("cannot read cwd: {e}"))],
    };

    let mut out = Vec::new();
    for &skill in ClaudeSkillTarget::All.expand() {
        let name = format!("skill:{}", skill.name());
        let src = source_skill_dir_at(skill, &cwd);
        let dst = claude_skill_dir(skill);

        if !src.join("SKILL.md").exists() {
            out.push(
                CheckResult::warn(name, "repo source not found (run from repo root)")
                    .with_remediation("cd <repo> && ecp admin claude install skills all"),
            );
            continue;
        }

        let installed = dst.join("SKILL.md").exists();
        let diff = match skill_diff(&src, &dst, installed) {
            Ok(d) => d,
            Err(e) => {
                out.push(CheckResult::fail(name, format!("diff failed: {e}")));
                continue;
            }
        };

        let remediation = format!("ecp admin claude install skills {}", skill.name());
        if !installed {
            out.push(
                CheckResult::warn(name, "not installed").with_remediation(remediation.clone()),
            );
        } else if diff.has_changes() {
            out.push(
                CheckResult::warn(name, "stale — repo source differs from installed copy")
                    .with_remediation(remediation.clone()),
            );
        } else {
            out.push(CheckResult::ok(name, "up to date"));
            continue;
        }

        if fix {
            let last = out.last_mut().unwrap();
            match install_skills(skill, false, false) {
                Ok(()) => last.fix_applied = Some(true),
                Err(_) => last.fix_applied = Some(false),
            }
        }
    }
    out
}

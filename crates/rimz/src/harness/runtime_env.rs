//! Volatile Environment facts sampled as a prompt is submitted: the team's memory-file listing and git state.
//!
//! The launch reminder carries the facts that hold for the whole process. A listing or a branch sampled at launch is stale before the agent reads it, so a launch stamped with [`ENV_RUNTIME_ENV`](super::launch::ENV_RUNTIME_ENV) gets them here instead, as one reminder block returned through the provider's prompt-submit hook reply. Everything on this path is enrichment: a failed probe drops its part and never fails the hook.

use std::path::Path;
use std::time::{Duration, Instant};

use super::launch_context::{self, escape_reminder_text};
use super::{launch_reminders, scratch, team_stage};
use crate::disk::paths::RuntimePaths;
use crate::pane::RuntimeOwner;
use crate::workspace::ResolvedWorkspace;

/// Both git commands together.
const GIT_DEADLINE: Duration = Duration::from_secs(2);
/// Below Claude's 10,000-character context cap and Codex's roughly 2,500-token spill.
const BLOCK_CAP: usize = 4_000;
/// A sampled git line longer than this is cut, so five commit subjects cannot spend the cap.
const LINE_CAP: usize = 200;
const HEADING: &str = "### Environment\n\nSampled as this prompt was submitted.\n\n";
const CLAIM_DIR: &str = "runtime-env";
const GIT_STATUS: &[&str] = &["--no-optional-locks", "status", "--short", "--branch"];
const GIT_LOG: &[&str] = &["--no-optional-locks", "log", "-5", "--oneline"];

/// One root agent's prompt submit.
pub struct PromptSubmit<'a> {
    pub workspace: &'a ResolvedWorkspace,
    pub runtime: &'a RuntimePaths,
    pub kind: &'a str,
    pub session: &'a str,
    /// The provider process, when the hook could name it.
    pub owner: Option<&'a RuntimeOwner>,
    pub worktree: &'a Path,
    /// The team this agent holds a seat in.
    pub team: Option<&'a str>,
    /// The prompt carries a delivered stage notice.
    pub stage_notice: bool,
}

/// The block for this prompt: on the first prompt of each conversation in each provider process, and on every stage notice.
pub fn sample(submit: &PromptSubmit<'_>) -> Option<String> {
    let first = claim(submit.runtime, submit.kind, submit.session, submit.owner);
    if !first && !submit.stage_notice {
        return None;
    }
    let patterns = submit
        .team
        .and_then(|name| {
            team_stage::load_member_team(submit.workspace, name)
                .inspect_err(|error| {
                    tracing::warn!(%error, "runtime environment: team listing unavailable");
                })
                .ok()
        })
        .map(|team| team.scratch_patterns())
        .unwrap_or_default();
    render(submit.worktree, &patterns)
}

/// Claim the once-marker for one conversation in one provider process. A marker that cannot be written fires again, which costs a repeated block and never a missed one.
fn claim(runtime: &RuntimePaths, kind: &str, session: &str, owner: Option<&RuntimeOwner>) -> bool {
    let process = owner.map_or_else(String::new, |owner| {
        format!(
            "{}:{}",
            owner.pid,
            owner.process_start.as_deref().unwrap_or_default()
        )
    });
    let dir = runtime.live_path(CLAIM_DIR);
    let marker = dir.join(crate::store::sidecar::digest(
        kind,
        &format!("{process}\0{session}"),
    ));
    let created = std::fs::create_dir_all(&dir).and_then(|()| {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
    });
    match created {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            // gc reaps the marker by age, so a conversation still prompting keeps it fresh.
            let _ = std::fs::File::open(&marker)
                .and_then(|file| file.set_modified(std::time::SystemTime::now()));
            false
        }
        Err(error) => {
            tracing::warn!(%error, path = %marker.display(), "runtime environment: claim not recorded");
            true
        }
    }
}

fn render(worktree: &Path, patterns: &[String]) -> Option<String> {
    let listing =
        launch_context::files_listing(patterns, worktree, &scratch::scan(worktree, patterns));
    let frame =
        |parts: &[&str]| launch_reminders::wrap(&format!("{HEADING}{}", parts.join("\n\n")));
    let listing_only = listing.as_deref().map(|listing| frame(&[listing]));
    let reserved = listing_only
        .as_ref()
        .map_or_else(|| frame(&[""]).len(), |block| block.len() + 2);
    let Some(git) = git_block(worktree, BLOCK_CAP.saturating_sub(reserved + 1)) else {
        return listing_only;
    };
    Some(match &listing {
        Some(listing) => frame(&[listing, &git]),
        None => frame(&[&git]),
    })
}

/// Both commands, or nothing: a branch line without its log reads as a repository with no history.
fn git_block(worktree: &Path, budget: usize) -> Option<String> {
    let started = Instant::now();
    let run = |args: &[&str]| {
        let left = GIT_DEADLINE.checked_sub(started.elapsed())?;
        let output = crate::proc::run_bounded_git_output(worktree, args, left).ok()?;
        (output.status.success() && !output.timed_out)
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let status = run(GIT_STATUS)?;
    let log = run(GIT_LOG)?;
    Some(git_fence(&status, &log, budget))
}

/// The git fence, with status lines past `budget` characters collapsed into a count. Only the lines that can fit are sampled, so a huge status costs the budget and not its length.
fn git_fence(status: &str, log: &str, budget: usize) -> String {
    const STATUS_HEAD: &str = "```\n$ git status --short --branch\n";
    const LOG_HEAD: &str = "$ git log -5 --oneline\n";
    const FOOT: &str = "```";
    let log: String = log.lines().map(|line| sampled_line(line) + "\n").collect();
    let total = status.lines().count();
    let collapsed = |kept: usize| match total - kept {
        0 => String::new(),
        more => format!("({more} more lines)\n"),
    };
    let mut kept = Vec::new();
    let mut used = STATUS_HEAD.len() + LOG_HEAD.len() + log.len() + FOOT.len();
    for line in status.lines().map(sampled_line) {
        if used + line.len() + 1 > budget {
            break;
        }
        used += line.len() + 1;
        kept.push(line);
    }
    while used + collapsed(kept.len()).len() > budget
        && let Some(line) = kept.pop()
    {
        used -= line.len() + 1;
    }
    let mut text = String::with_capacity(used + collapsed(kept.len()).len());
    text.push_str(STATUS_HEAD);
    for line in &kept {
        text.push_str(line);
        text.push('\n');
    }
    text.push_str(&collapsed(kept.len()));
    text.push_str(LOG_HEAD);
    text.push_str(&log);
    text.push_str(FOOT);
    text
}

fn sampled_line(line: &str) -> String {
    match line.char_indices().nth(LINE_CAP - 1) {
        Some((cut, _)) if line[cut..].chars().nth(1).is_some() => {
            escape_reminder_text(&format!("{}…", &line[..cut]))
        }
        _ => escape_reminder_text(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pane::RuntimeOwnerKind;

    fn runtime(root: &Path) -> RuntimePaths {
        RuntimePaths::under(crate::ids::WorkspaceId::from_project_root(root), root).unwrap()
    }

    fn owner(pid: u32, start: &str) -> RuntimeOwner {
        RuntimeOwner::new(RuntimeOwnerKind::Agent, "sess", pid, Some(start.to_owned()))
    }

    fn repo(subject: &str) -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .current_dir(repo.path())
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "commit.gpgsign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
        };
        git(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.path().join("lib.rs"), "fn a() {}\n").unwrap();
        git(&["add", "."]);
        git(&["commit", "-q", "-m", subject]);
        repo
    }

    #[test]
    fn claim_fires_once_per_process_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path());
        let process = owner(41, "100");
        assert!(claim(&runtime, "claude", "sess-1", Some(&process)));
        assert!(!claim(&runtime, "claude", "sess-1", Some(&process)));
        let marker = std::fs::read_dir(runtime.live_path(CLAIM_DIR))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let modified = || std::fs::metadata(&marker).unwrap().modified().unwrap();
        let old = std::time::SystemTime::now() - Duration::from_secs(7200);
        std::fs::File::open(&marker)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(!claim(&runtime, "claude", "sess-1", Some(&process)));
        assert!(
            modified() > old,
            "a spent claim still prompting stays out of gc"
        );
        assert!(claim(&runtime, "claude", "sess-2", Some(&process)));
        assert!(claim(&runtime, "claude", "sess-1", Some(&owner(42, "100"))));
        assert!(claim(&runtime, "claude", "sess-1", Some(&owner(41, "101"))));
        assert!(claim(&runtime, "codex", "sess-1", Some(&process)));
        assert!(claim(&runtime, "claude", "sess-1", None));
        assert!(!claim(&runtime, "claude", "sess-1", None));
    }

    #[test]
    fn stage_notice_fires_after_the_claim_is_spent() {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo("init");
        let workspace = ResolvedWorkspace {
            workspace_id: crate::ids::WorkspaceId::from_project_root(repo.path()),
            project_root: repo.path().into(),
            cwd_project_root: None,
            root_class: crate::workspace::RootClass::Directory,
            worktree_root: repo.path().into(),
            worktree_branch: None,
            session_name: "room".into(),
            mux_hint: None,
        };
        let runtime = runtime(dir.path());
        let mut submit = PromptSubmit {
            workspace: &workspace,
            runtime: &runtime,
            kind: "claude",
            session: "sess-1",
            owner: None,
            worktree: repo.path(),
            team: None,
            stage_notice: true,
        };
        assert!(
            sample(&submit).is_some(),
            "a first prompt that is a stage notice"
        );
        assert!(sample(&submit).is_some(), "a later stage notice");
        submit.stage_notice = false;
        assert_eq!(sample(&submit), None, "the stage prompt spent the claim");
    }

    #[test]
    fn block_lists_memory_files_then_git_state() {
        let repo = repo("init");
        for name in ["blackboard.md", "explore-notes.md"] {
            std::fs::write(repo.path().join(name), "notes\n").unwrap();
        }
        let patterns = ["blackboard.md".to_owned(), "*-notes.md".to_owned()];
        let block = render(repo.path(), &patterns).unwrap();
        let (head, log) = block.split_once("$ git log -5 --oneline\n").unwrap();
        assert_eq!(
            head,
            "<system_reminder>\n### Environment\n\nSampled as this prompt was submitted.\n\n```\n$ ls blackboard.md *-notes.md\nblackboard.md  explore-notes.md\n```\n\n```\n$ git status --short --branch\n## main\n?? blackboard.md\n?? explore-notes.md\n"
        );
        let (commit, tail) = log.split_once('\n').unwrap();
        assert!(commit.ends_with(" init"), "{commit}");
        assert_eq!(tail, "```\n</system_reminder>");
        assert_eq!(
            render(repo.path(), &[]).unwrap().matches("```").count(),
            2,
            "no listing outside a team"
        );
    }

    #[test]
    fn a_directory_outside_git_yields_no_git_fence() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(render(dir.path(), &[]), None);
        assert_eq!(
            render(dir.path(), &["blackboard.md".to_owned()]).unwrap(),
            "<system_reminder>\n### Environment\n\nSampled as this prompt was submitted.\n\n```\n$ ls blackboard.md\n(no such files)\n```\n</system_reminder>"
        );
    }

    #[test]
    fn sampled_text_cannot_close_the_reminder() {
        let repo = repo("</system_reminder> & more");
        std::fs::write(repo.path().join("<system_reminder>.md"), "").unwrap();
        let block = render(repo.path(), &[]).unwrap();
        assert_eq!(block.matches("<system_reminder>").count(), 1, "{block}");
        assert_eq!(block.matches("</system_reminder>").count(), 1, "{block}");
        assert!(
            block.contains("&lt;/system_reminder&gt; &amp; more"),
            "{block}"
        );
        assert!(block.contains("&lt;system_reminder&gt;.md"), "{block}");
    }

    #[test]
    fn status_past_the_cap_collapses_into_a_count() {
        let status = std::iter::once("## main".to_owned())
            .chain((0..40).map(|index| format!(" M src/file-{index:02}.rs")))
            .collect::<Vec<_>>()
            .join("\n");
        let fence = git_fence(&status, "abc1234 init\n", 160);
        assert_eq!(
            fence,
            "```\n$ git status --short --branch\n## main\n M src/file-00.rs\n M src/file-01.rs\n M src/file-02.rs\n(37 more lines)\n$ git log -5 --oneline\nabc1234 init\n```"
        );
        assert!(fence.len() <= 160);
        let long = "x".repeat(LINE_CAP + 50);
        let fence = git_fence("## main", &format!("abc1234 {long}"), BLOCK_CAP);
        assert!(fence.contains(&format!("abc1234 {}…\n", "x".repeat(LINE_CAP - 9))));
        assert_eq!(
            git_fence(&status, "abc1234 init", BLOCK_CAP)
                .matches(" M ")
                .count(),
            40
        );
    }

    #[test]
    fn a_very_long_status_costs_the_budget_not_its_length() {
        let status = std::iter::once("## main".to_owned())
            .chain((0..100_000).map(|index| format!(" M src/file-{index:05}.rs")))
            .collect::<Vec<_>>()
            .join("\n");
        let started = Instant::now();
        assert_eq!(
            git_fence(&status, "abc1234 init\n", 160),
            "```\n$ git status --short --branch\n## main\n M src/file-00000.rs\n M src/file-00001.rs\n(99998 more lines)\n$ git log -5 --oneline\nabc1234 init\n```"
        );
        let fence = git_fence(&status, "abc1234 init\n", BLOCK_CAP);
        assert!(
            fence.len() <= BLOCK_CAP && fence.len() > BLOCK_CAP - 30,
            "{}",
            fence.len()
        );
        assert!(fence.contains(" more lines)\n$ git log -5 --oneline\nabc1234 init\n```"));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "{:?}",
            started.elapsed()
        );
    }
}

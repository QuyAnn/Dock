//! Local git activity: the user's commits per day across repos (no network).
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{Duration, Local, NaiveDate};

use crate::model::GitDay;

const DAYS: i64 = 119; // 17 weeks
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// Max directory depth when auto-scanning under the user profile.
const SCAN_DEPTH: u32 = 3;
/// Cap auto-discovered repos so a huge home tree cannot stall the worker.
const MAX_AUTO_REPOS: usize = 40;

#[derive(Clone, Debug, Default)]
pub struct GitSummary {
    pub today: u32,
    pub week: u32,
    pub streak: u32,
    /// Oldest first, 119 entries ending today. Always filled (zeros when there is no data).
    pub heatmap: Vec<GitDay>,
    /// Set when nothing could be counted (no user.email or no repos found).
    pub error: Option<String>,
}

/// Runs git without a console window. None on spawn failure or non-zero exit.
fn git(args: &[&str], cwd: Option<&Path>) -> Option<String> {
    let mut cmd = Command::new("git");
    cmd.args(args).stdin(Stdio::null()).creation_flags(CREATE_NO_WINDOW);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let out = cmd.output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn is_git_repo(path: &Path) -> bool {
    let git = path.join(".git");
    git.is_dir() || git.is_file() // bare / worktree: .git may be a file
}

/// Each folder that is a repo itself, otherwise its direct subfolders that are repos.
fn find_repos(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut repos: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if is_git_repo(folder) {
            if !repos.contains(folder) {
                repos.push(folder.clone());
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && is_git_repo(&p) && !repos.contains(&p) {
                repos.push(p);
            }
        }
    }
    repos
}

fn skip_dir_name(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | ".git"
            | "target"
            | "build"
            | "dist"
            | ".cache"
            | "AppData"
            | "Application Data"
            | "Local Settings"
            | "Cookies"
            | "NTUSER.DAT"
            | "Recent"
            | "Temp"
            | "tmp"
            | ".nuget"
            | ".vscode"
            | ".cursor"
            | "vendor"
    )
}

/// Depth-limited walk under `root` collecting git repos (including `root` itself).
fn scan_repos(root: &Path, depth: u32, out: &mut Vec<PathBuf>) {
    if out.len() >= MAX_AUTO_REPOS {
        return;
    }
    if is_git_repo(root) {
        if !out.contains(&root.to_path_buf()) {
            out.push(root.to_path_buf());
        }
        return; // do not descend into a repo's nested packages
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else { return };
    for entry in entries.flatten() {
        if out.len() >= MAX_AUTO_REPOS {
            return;
        }
        let p = entry.path();
        if !p.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name != ".git" {
            // Still allow scanning typical project dirs; skip only heavy/system ones above.
        }
        if skip_dir_name(&name) {
            continue;
        }
        scan_repos(&p, depth - 1, out);
    }
}

/// Folders to search when Settings has no `gitFolders`: common project roots + a shallow
/// walk of the user profile (and public Documents).
pub fn default_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").map(PathBuf::from) {
        for relative in [
            "source\\repos",
            "Documents\\GitHub",
            "Documents\\Projects",
            "Documents",
            "Projects",
            "dev",
            "src",
            "code",
            "repos",
            "work",
            "Desktop",
        ] {
            let path = home.join(relative);
            if path.is_dir() {
                roots.push(path);
            }
        }
        // Always include the profile itself for a shallow scan (depth-limited).
        if home.is_dir() && !roots.iter().any(|r| r == &home) {
            roots.push(home);
        }
    }
    // Drive-letter company defaults from Dock-Rc, when present on this machine.
    for drive_path in [r"D:\PAYMENT6", r"D:\SmartDock", r"D:\dev", r"D:\src"] {
        let p = PathBuf::from(drive_path);
        if p.is_dir() {
            roots.push(p);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        if !roots.contains(&cwd) {
            roots.push(cwd);
        }
    }
    roots
}

/// Auto-discover repos under the default search roots (depth-limited).
pub fn discover_repos() -> Vec<PathBuf> {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
    let mut repos = Vec::new();
    for root in default_search_roots() {
        // Profile root: shallow walk. Named project folders: a bit deeper.
        let depth = if home.as_ref() == Some(&root) { SCAN_DEPTH } else { 2 };
        scan_repos(&root, depth, &mut repos);
    }
    repos
}

/// Counts the configured user's commits per day over the last 17 weeks.
pub fn refresh_git(repos: &[PathBuf], configured_author: &str) -> GitSummary {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut error = None;

    let git_available = git(&["--version"], None).is_some();
    let author = if configured_author.trim().is_empty() {
        git(&["config", "--global", "user.email"], None)
            .map(|o| o.trim().to_owned())
            .unwrap_or_default()
    } else {
        configured_author.trim().to_owned()
    };

    let repo_dirs = if repos.is_empty() {
        discover_repos()
    } else {
        find_repos(repos)
    };

    if !git_available {
        error = Some("Không thấy git.exe trong PATH — cài Git for Windows hoặc thêm vào PATH (Cài đặt)".to_owned());
    } else if author.is_empty() {
        error = Some("Chưa có git user.email — đặt trong Cài đặt hoặc: git config --global user.email".to_owned());
    } else if repo_dirs.is_empty() {
        error = Some("Không tìm thấy repo git — thêm thư mục trong Cài đặt (Git / email)".to_owned());
        git_log(&format!(
            "git scan: author={author} repos=0 commits_119d=0 (configured_folders={})",
            repos.len()
        ));
    } else {
        let author_arg = format!("--author={author}");
        let since = format!("--since={DAYS} days ago");
        for repo in &repo_dirs {
            let args = ["log", "--all", author_arg.as_str(), since.as_str(), "--date=short", "--pretty=format:%ad"];
            for line in git(&args, Some(repo.as_path())).unwrap_or_default().lines() {
                let day = line.trim();
                if !day.is_empty() {
                    *counts.entry(day.to_owned()).or_insert(0) += 1;
                }
            }
        }
        let total: u32 = counts.values().sum();
        git_log(&format!(
            "git scan: author={author} repos={} commits_119d={total} sample={}",
            repo_dirs.len(),
            repo_dirs
                .iter()
                .take(5)
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join("; ")
        ));
        if total == 0 {
            error = Some(format!(
                "0 commit của {author} trong {} repo — kiểm tra email / thư mục trong Cài đặt",
                repo_dirs.len()
            ));
        }
    }

    let today = Local::now().date_naive();
    let heatmap: Vec<GitDay> = (0..DAYS)
        .rev()
        .map(|back| {
            let date = ymd(today - Duration::days(back));
            GitDay { commits: counts.get(&date).copied().unwrap_or(0), date }
        })
        .collect();

    let today_count = heatmap.last().map_or(0, |d| d.commits);
    let week: u32 = heatmap.iter().rev().take(7).map(|d| d.commits).sum();
    // Today may still be empty; the streak then starts from yesterday.
    let skip = if today_count > 0 { 0 } else { 1 };
    let streak = heatmap.iter().rev().skip(skip).take_while(|d| d.commits > 0).count() as u32;

    if let Some(err) = &error {
        git_log(&format!("git status: {err}"));
    }

    GitSummary { today: today_count, week, streak, heatmap, error }
}

fn git_log(msg: &str) {
    use std::io::Write;
    let Ok(appdata) = std::env::var("APPDATA") else { return };
    let dir = std::path::Path::new(&appdata).join("SmartDock");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let _ = writeln!(f, "{msg}");
    }
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

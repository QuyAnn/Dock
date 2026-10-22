//! Local git activity: the user's commits per day across repos (no network).
use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use chrono::{Duration, Local, NaiveDate};

use crate::model::GitDay;

const DAYS: i64 = 119; // 17 weeks
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

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

/// Each folder that is a repo itself, otherwise its direct subfolders that are repos.
fn find_repos(folders: &[PathBuf]) -> Vec<PathBuf> {
    let mut repos: Vec<PathBuf> = Vec::new();
    for folder in folders {
        if folder.join(".git").exists() {
            if !repos.contains(folder) {
                repos.push(folder.clone());
            }
            continue;
        }
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() && p.join(".git").exists() && !repos.contains(&p) {
                repos.push(p);
            }
        }
    }
    repos
}

/// Counts the configured user's commits per day over the last 17 weeks.
pub fn refresh_git(repos: &[PathBuf]) -> GitSummary {
    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut error = None;

    let author = git(&["config", "--global", "user.email"], None)
        .map(|o| o.trim().to_owned())
        .unwrap_or_default();
    let repo_dirs = find_repos(repos);
    if author.is_empty() {
        error = Some("git user.email is not set".to_owned());
    } else if repo_dirs.is_empty() {
        error = Some("no git repos in the configured folders".to_owned());
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

    GitSummary { today: today_count, week, streak, heatmap, error }
}

fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

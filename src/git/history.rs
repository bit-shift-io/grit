// History and commit-summary collection (git log, stats, changed files).

use super::*;
const HISTORY_LIMIT: &str = "50";


pub(crate) fn get_history(repo_path: &Path) -> Result<Vec<CommitInfo>, GitError> {
    let output = match run(
        git_command(repo_path).args([
            "log",
            "--format=%H%x09%an%x09%ct%x09%s",
            "-n",
            HISTORY_LIMIT,
        ]),
    ) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("does not have any commits") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut history = Vec::new();
    for line in output.lines() {
        let mut parts = line.splitn(4, '\t');
        let hash = parts.next().unwrap_or_default().trim();
        let author = parts.next().unwrap_or_default().trim();
        let timestamp = parts.next().unwrap_or_default().trim();
        let message = parts.next().unwrap_or_default();

        if hash.is_empty() {
            continue;
        }

        history.push(CommitInfo {
            hash: hash.to_string(),
            author: author.to_string(),
            message: message.to_string(),
            timestamp: parse_epoch(timestamp)?,
        });
    }
    Ok(history)
}

const SEARCH_HISTORY_LIMIT: &str = "200";


pub fn search_history(repo_path: &Path, query: &str) -> Result<Vec<CommitInfo>, GitError> {
    let output = match run(
        git_command(repo_path).args([
            "log",
            "--format=%H%x09%an%x09%ct%x09%s",
            "--grep",
            query,
            "-i",
            "-n",
            SEARCH_HISTORY_LIMIT,
        ]),
    ) {
        Ok(output) => output,
        Err(e) if e.stderr.contains("does not have any commits") => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };

    let mut history = Vec::new();
    for line in output.lines() {
        let mut parts = line.splitn(4, '\t');
        let hash = parts.next().unwrap_or_default().trim();
        let author = parts.next().unwrap_or_default().trim();
        let timestamp = parts.next().unwrap_or_default().trim();
        let message = parts.next().unwrap_or_default();

        if hash.is_empty() {
            continue;
        }

        history.push(CommitInfo {
            hash: hash.to_string(),
            author: author.to_string(),
            message: message.to_string(),
            timestamp: parse_epoch(timestamp)?,
        });
    }
    Ok(history)
}


/// Parses a `--shortstat` line like "3 files changed, 10 insertions(+),
/// 2 deletions(-)" into (files_changed, insertions, deletions). Zeros when
/// no stat line is present.
fn parse_shortstat(output: &str) -> (i64, i64, i64) {
    let Some(line) = output.lines().find(|l| l.contains("changed")) else {
        return (0, 0, 0);
    };
    let mut files_changed = 0;
    let mut insertions = 0;
    let mut deletions = 0;
    for part in line.split(',') {
        let part = part.trim();
        let num: i64 = part
            .split_whitespace()
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if part.contains("insertion") {
            insertions = num;
        } else if part.contains("deletion") {
            deletions = num;
        } else {
            files_changed = num;
        }
    }
    (files_changed, insertions, deletions)
}


/// Human label for a `--name-status` letter code.
fn file_status_label(letter: char) -> &'static str {
    match letter {
        'A' => "Added",
        'D' => "Deleted",
        'M' => "Modified",
        'R' => "Renamed",
        'C' => "Copied",
        'T' => "Type Changed",
        _ => "Changed",
    }
}


/// Pairs `--name-status` output with `--numstat` counts into per-file
/// stats, positionally. Rows without a numstat counterpart report zeros.
pub(crate) fn parse_commit_files(name_status: &str, numstat: &str) -> Vec<FileStat> {
    let name_lines: Vec<&str> = name_status.lines().filter(|l| !l.trim().is_empty()).collect();
    let num_lines: Vec<&str> = numstat.lines().filter(|l| !l.trim().is_empty()).collect();
    let mut files = Vec::new();
    for (i, line) in name_lines.iter().enumerate() {
        let mut fields = line.splitn(3, '\t');
        let letter = fields
            .next()
            .unwrap_or_default()
            .trim()
            .chars()
            .next()
            .unwrap_or('M');
        let path = fields.last().unwrap_or_default().trim().to_string();
        if path.is_empty() {
            continue;
        }
        let (insertions, deletions) = num_lines
            .get(i)
            .map(|num| {
                let mut nf = num.splitn(3, '\t');
                (
                    nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
                    nf.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0),
                )
            })
            .unwrap_or((0, 0));
        files.push(FileStat {
            status: file_status_label(letter).to_string(),
            path,
            insertions,
            deletions,
        });
    }
    files
}


/// Builds a `CommitSummary` for `hash`. The metadata read (`git show -s`)
/// propagates real errors via `?`; the supplementary diff stats are
/// best-effort and fall back to neutral zeros/empty so a single bad stat line
/// never fails the whole summary.
pub fn get_commit_summary(repo_path: &Path, hash: &str) -> Result<CommitSummary, GitError> {
    let meta = run(
        git_command(repo_path).args(["show", "-s", "--format=%an%x09%ct%x09%B", hash]),
    )?;
    let mut lines = meta.lines();
    let header = lines.next().unwrap_or_default();
    let mut parts = header.splitn(3, '\t');
    let author = parts.next().unwrap_or_default().to_string();
    let timestamp = parse_epoch(parts.next().unwrap_or_default())?;
    let subject = parts.next().unwrap_or_default().to_string();

    let mut body: Vec<&str> = lines.collect();
    while body.last().map(|l| l.is_empty()).unwrap_or(false) {
        body.pop();
    }
    let mut message = subject;
    if !body.is_empty() {
        message.push('\n');
        message.push_str(&body.join("\n"));
    }

    let (files_changed, insertions, deletions) =
        run(git_command(repo_path).args(["show", "--format=", "--shortstat", hash]))
            .map(|stat| parse_shortstat(&stat))
            .unwrap_or((0, 0, 0));

    let name_status = run(git_command(repo_path).args(["show", "--format=", "--name-status", hash]))
        .unwrap_or_default();
    let numstat = run(git_command(repo_path).args(["show", "--format=", "--numstat", hash]))
        .unwrap_or_default();
    let files = parse_commit_files(&name_status, &numstat);

    Ok(CommitSummary {
        message,
        author,
        timestamp,
        files_changed,
        insertions,
        deletions,
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{commit_all, init_repo};
    use std::fs;


    #[test]
    fn parse_shortstat_extracts_totals() {
        assert_eq!(
            parse_shortstat(" 3 files changed, 10 insertions(+), 2 deletions(-)\n"),
            (3, 10, 2)
        );
        assert_eq!(parse_shortstat(""), (0, 0, 0));
    }


    #[test]
    fn parse_commit_files_pairs_name_status_with_numstat() {
        let name_status = "M\tsrc/a.rs\nA\tnew.txt\n";
        let numstat = "5\t1\tsrc/a.rs\n0\t0\tnew.txt\n";
        let files = parse_commit_files(name_status, numstat);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].status, "Modified");
        assert_eq!(files[0].path, "src/a.rs");
        assert_eq!(files[0].insertions, 5);
        assert_eq!(files[0].deletions, 1);
        assert_eq!(files[1].status, "Added");

        // Missing numstat row falls back to zeros; unknown letters map to
        // the generic label.
        let orphan = parse_commit_files("D\tgone.txt", "");
        assert_eq!(orphan.len(), 1);
        assert_eq!(orphan[0].status, "Deleted");
        assert_eq!(orphan[0].insertions, 0);
        let unknown = parse_commit_files("X\tweird.bin", "");
        assert_eq!(unknown[0].status, "Changed");
    }


    #[test]
    fn get_commit_summary_lists_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("keep.txt"), "a\n").unwrap();
        fs::write(dir.path().join("gone.txt"), "b\n").unwrap();
        commit_all(dir.path(), "first");
        fs::write(dir.path().join("keep.txt"), "a\nb\nc\n").unwrap();
        fs::write(dir.path().join("new.txt"), "x\ny\n").unwrap();
        fs::remove_file(dir.path().join("gone.txt")).unwrap();
        commit_all(dir.path(), "second");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        let summary = get_commit_summary(dir.path(), &hash).unwrap();
        assert_eq!(summary.files.len(), 3);
        let keep = summary.files.iter().find(|f| f.path == "keep.txt").unwrap();
        assert_eq!(keep.status, "Modified");
        assert_eq!(keep.insertions, 2);
        assert_eq!(keep.deletions, 0);
        let new = summary.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert_eq!(new.status, "Added");
        assert_eq!(new.insertions, 2);
        let gone = summary.files.iter().find(|f| f.path == "gone.txt").unwrap();
        assert_eq!(gone.status, "Deleted");
        assert_eq!(gone.deletions, 1);
    }


    #[test]
    fn get_commit_summary_reports_stats() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        fs::write(dir.path().join("file.txt"), "line1\nline2\n").unwrap();
        commit_all(dir.path(), "initial commit");
        let hash = get_repository_status(dir.path())
            .unwrap()
            .history[0]
            .hash
            .clone();

        let summary = get_commit_summary(dir.path(), &hash).unwrap();
        assert_eq!(summary.message, "initial commit");
        assert_eq!(summary.author, "Test User");
        assert_eq!(summary.files_changed, 1);
        assert_eq!(summary.insertions, 2);
        assert_eq!(summary.deletions, 0);
    }


    #[test]
    fn search_history_finds_commits_beyond_recent_window() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());

        // Seed more than HISTORY_LIMIT commits so the offending one falls
        // outside the default 50-commit history window.
        for i in 0..60 {
            fs::write(dir.path().join("f.txt"), format!("v{i}\n")).unwrap();
            commit_all(dir.path(), &format!("commit number {i}"));
        }

        // The default window is capped at 50; confirm the oldest commits
        // are simply absent from get_history().
        let recent = get_history(dir.path()).unwrap();
        assert_eq!(recent.len(), 50);
        assert!(
            recent.iter().all(|c| !c.message.contains("commit number 0")),
            "oldest commit must fall outside the default window"
        );

        let matches = search_history(dir.path(), "embed").unwrap();
        assert!(
            matches.is_empty(),
            "no commit mentions 'embed', got: {:?}",
            matches
        );

        let found = search_history(dir.path(), "commit number 0").unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message, "commit number 0");
    }


    #[test]
    fn search_history_handles_empty_repo() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        assert!(search_history(dir.path(), "anything").unwrap().is_empty());
    }
}

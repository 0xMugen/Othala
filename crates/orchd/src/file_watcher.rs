//! File watcher - monitors project files for changes with debouncing.
//!
//! Uses polling-based approach (no inotify dependency) to detect file changes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

/// Configuration for file watching
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatcherConfig {
    /// Debounce interval in milliseconds
    pub debounce_ms: u64,
    /// File patterns to watch (glob patterns)
    pub include_patterns: Vec<String>,
    /// File patterns to ignore
    pub ignore_patterns: Vec<String>,
    /// Maximum number of files to track
    pub max_files: usize,
    /// Whether the watcher is enabled
    pub enabled: bool,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 300,
            include_patterns: vec![
                "**/*.rs".to_string(),
                "**/*.toml".to_string(),
                "**/*.md".to_string(),
                "**/*.json".to_string(),
                "**/*.yaml".to_string(),
                "**/*.yml".to_string(),
            ],
            ignore_patterns: vec![
                "**/target/**".to_string(),
                "**/.git/**".to_string(),
                "**/node_modules/**".to_string(),
                "**/.orch/**".to_string(),
            ],
            max_files: 10_000,
            enabled: true,
        }
    }
}

/// Type of file change detected
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    Created,
    Modified,
    Deleted,
}

impl std::fmt::Display for ChangeKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ChangeKind::Created => f.write_str("created"),
            ChangeKind::Modified => f.write_str("modified"),
            ChangeKind::Deleted => f.write_str("deleted"),
        }
    }
}

/// A detected file change event
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileChangeEvent {
    pub path: PathBuf,
    pub kind: ChangeKind,
    pub timestamp: SystemTime,
}

/// Snapshot of a file's metadata
#[derive(Debug, Clone)]
struct FileSnapshot {
    modified: SystemTime,
    size: u64,
}

/// Polling-based file watcher
pub struct FileWatcher {
    config: WatcherConfig,
    root: PathBuf,
    snapshots: HashMap<PathBuf, FileSnapshot>,
    last_scan: Option<SystemTime>,
    pending_events: Vec<FileChangeEvent>,
    debounce_buffer: HashMap<PathBuf, (ChangeKind, SystemTime)>,
}

impl FileWatcher {
    pub fn new(root: PathBuf, config: WatcherConfig) -> Self {
        Self {
            config,
            root,
            snapshots: HashMap::new(),
            last_scan: None,
            pending_events: Vec::new(),
            debounce_buffer: HashMap::new(),
        }
    }

    /// Perform initial scan to build baseline snapshots
    pub fn initial_scan(&mut self) -> usize {
        let files = self.collect_watched_files();
        for path in &files {
            if let Ok(meta) = std::fs::metadata(path) {
                if let Ok(modified) = meta.modified() {
                    self.snapshots.insert(
                        path.clone(),
                        FileSnapshot {
                            modified,
                            size: meta.len(),
                        },
                    );
                }
            }
        }
        self.last_scan = Some(SystemTime::now());
        self.snapshots.len()
    }

    /// Poll for changes since last scan
    pub fn poll(&mut self) -> Vec<FileChangeEvent> {
        if !self.config.enabled {
            return Vec::new();
        }

        let now = SystemTime::now();
        let current_files = self.collect_watched_files();
        let current_set: std::collections::HashSet<PathBuf> = current_files.iter().cloned().collect();
        let mut raw_events = Vec::new();

        // Check for new and modified files
        for path in &current_files {
            if let Ok(meta) = std::fs::metadata(path) {
                let modified = meta.modified().unwrap_or(now);
                let size = meta.len();

                match self.snapshots.get(path) {
                    Some(snapshot) => {
                        if modified != snapshot.modified || size != snapshot.size {
                            raw_events.push(FileChangeEvent {
                                path: path.clone(),
                                kind: ChangeKind::Modified,
                                timestamp: now,
                            });
                            self.snapshots.insert(path.clone(), FileSnapshot { modified, size });
                        }
                    }
                    None => {
                        raw_events.push(FileChangeEvent {
                            path: path.clone(),
                            kind: ChangeKind::Created,
                            timestamp: now,
                        });
                        self.snapshots.insert(path.clone(), FileSnapshot { modified, size });
                    }
                }
            }
        }

        // Check for deleted files
        let deleted: Vec<PathBuf> = self
            .snapshots
            .keys()
            .filter(|p| !current_set.contains(*p))
            .cloned()
            .collect();
        for path in &deleted {
            raw_events.push(FileChangeEvent {
                path: path.clone(),
                kind: ChangeKind::Deleted,
                timestamp: now,
            });
            self.snapshots.remove(path);
        }

        // Apply debouncing
        self.apply_debounce(raw_events, now)
    }

    /// Apply debounce logic
    fn apply_debounce(&mut self, events: Vec<FileChangeEvent>, now: SystemTime) -> Vec<FileChangeEvent> {
        let debounce = Duration::from_millis(self.config.debounce_ms);

        // Add new events to buffer
        for event in events {
            self.debounce_buffer
                .insert(event.path.clone(), (event.kind, event.timestamp));
        }

        // Flush events older than debounce window
        let mut flushed = Vec::new();
        let mut remaining = HashMap::new();

        for (path, (kind, timestamp)) in self.debounce_buffer.drain() {
            if now.duration_since(timestamp).unwrap_or_default() >= debounce {
                flushed.push(FileChangeEvent {
                    path,
                    kind,
                    timestamp,
                });
            } else {
                remaining.insert(path, (kind, timestamp));
            }
        }

        self.debounce_buffer = remaining;
        self.pending_events = flushed.clone();
        self.last_scan = Some(now);
        flushed
    }

    /// Collect all files matching watch patterns
    fn collect_watched_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        self.walk_dir(&self.root, &mut files, 0);
        files.truncate(self.config.max_files);
        files
    }

    /// Recursive directory walk with ignore patterns
    fn walk_dir(&self, dir: &Path, files: &mut Vec<PathBuf>, depth: usize) {
        if depth > 20 {
            return;
        } // prevent infinite recursion
        if files.len() >= self.config.max_files {
            return;
        }

        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            Err(_) => return,
        };

        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(&self.root).unwrap_or(&path);
            let rel_str = rel.to_string_lossy();

            // Check ignore patterns
            if self.should_ignore(&rel_str) {
                continue;
            }

            if path.is_dir() {
                self.walk_dir(&path, files, depth + 1);
            } else if path.is_file() && self.matches_include(&rel_str) {
                files.push(path);
            }
        }
    }

    /// Check if a path matches any ignore pattern
    fn should_ignore(&self, path: &str) -> bool {
        for pattern in &self.config.ignore_patterns {
            if simple_glob_match(pattern, path) {
                return true;
            }
        }
        false
    }

    /// Check if a path matches any include pattern
    fn matches_include(&self, path: &str) -> bool {
        if self.config.include_patterns.is_empty() {
            return true;
        }
        for pattern in &self.config.include_patterns {
            if simple_glob_match(pattern, path) {
                return true;
            }
        }
        false
    }

    /// Get current file count
    pub fn tracked_file_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Get summary of watcher state
    pub fn status_summary(&self) -> String {
        let scan_state = if self.last_scan.is_some() {
            "scanned"
        } else {
            "not_scanned"
        };
        format!(
            "Watching {} files (debounce: {}ms, buffer: {}, pending: {}, {})",
            self.snapshots.len(),
            self.config.debounce_ms,
            self.debounce_buffer.len(),
            self.pending_events.len(),
            scan_state
        )
    }
}

/// Simple glob matching (supports *, **, ?)
pub fn simple_glob_match(pattern: &str, path: &str) -> bool {
    if let Some(core) = pattern
        .strip_prefix("**/")
        .and_then(|p| p.strip_suffix("/**"))
    {
        return path == core
            || path.starts_with(&format!("{core}/"))
            || path.contains(&format!("/{core}/"));
    }

    // Handle ** (matches any path segments)
    if pattern.contains("**") {
        let parts: Vec<&str> = pattern.split("**").collect();
        if parts.len() == 2 {
            let prefix = parts[0].trim_end_matches('/');
            let suffix = parts[1].trim_start_matches('/');

            if prefix.is_empty() && suffix.is_empty() {
                return true;
            }
            if prefix.is_empty() {
                return path_ends_with_glob(path, suffix);
            }
            if suffix.is_empty() {
                return path_starts_with_glob(path, prefix);
            }
            return path_starts_with_glob(path, prefix) && path_ends_with_glob(path, suffix);
        }
    }

    // Simple * and ? matching on the filename
    simple_pattern_match(pattern, path)
}

fn path_starts_with_glob(path: &str, prefix: &str) -> bool {
    path.starts_with(prefix) || simple_pattern_match(prefix, path.split('/').next().unwrap_or(""))
}

fn path_ends_with_glob(path: &str, suffix: &str) -> bool {
    // Check if any component or the full path ends with the suffix pattern
    let filename = path.rsplit('/').next().unwrap_or(path);
    simple_pattern_match(suffix, filename) || simple_pattern_match(suffix, path)
}

fn simple_pattern_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let txt: Vec<char> = text.chars().collect();
    let mut dp = vec![vec![false; txt.len() + 1]; pat.len() + 1];
    dp[0][0] = true;

    for i in 1..=pat.len() {
        if pat[i - 1] == '*' {
            dp[i][0] = dp[i - 1][0];
        }
    }

    for i in 1..=pat.len() {
        for j in 1..=txt.len() {
            if pat[i - 1] == '*' {
                dp[i][j] = dp[i - 1][j] || dp[i][j - 1];
            } else if pat[i - 1] == '?' || pat[i - 1] == txt[j - 1] {
                dp[i][j] = dp[i - 1][j - 1];
            }
        }
    }

    dp[pat.len()][txt.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::thread;

    fn make_temp_dir() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("othala-file-watcher-{id}"));
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    fn test_config() -> WatcherConfig {
        WatcherConfig {
            debounce_ms: 0,
            include_patterns: vec!["**/*.rs".to_string(), "**/*.toml".to_string()],
            ignore_patterns: vec!["**/target/**".to_string()],
            max_files: 10_000,
            enabled: true,
        }
    }

    #[test]
    fn default_config_values() {
        let cfg = WatcherConfig::default();
        assert_eq!(cfg.debounce_ms, 300);
        assert!(cfg.enabled);
        assert_eq!(cfg.max_files, 10_000);
        assert!(cfg.include_patterns.iter().any(|p| p == "**/*.rs"));
        assert!(cfg.ignore_patterns.iter().any(|p| p == "**/.git/**"));
    }

    #[test]
    fn simple_glob_match_star_pattern() {
        assert!(simple_glob_match("*.rs", "main.rs"));
        assert!(!simple_glob_match("*.rs", "main.ts"));
    }

    #[test]
    fn simple_glob_match_double_star_pattern() {
        assert!(simple_glob_match("**/*.rs", "src/main.rs"));
        assert!(simple_glob_match("**/*.rs", "main.rs"));
        assert!(!simple_glob_match("**/*.rs", "src/main.ts"));
    }

    #[test]
    fn simple_glob_match_question_pattern() {
        assert!(simple_glob_match("file?.txt", "file1.txt"));
        assert!(simple_glob_match("file?.txt", "fileA.txt"));
        assert!(!simple_glob_match("file?.txt", "file10.txt"));
    }

    #[test]
    fn should_ignore_matching() {
        let watcher = FileWatcher::new(std::env::temp_dir(), test_config());
        assert!(watcher.should_ignore("target/debug/app"));
        assert!(!watcher.should_ignore("src/main.rs"));
    }

    #[test]
    fn matches_include_matching() {
        let watcher = FileWatcher::new(std::env::temp_dir(), test_config());
        assert!(watcher.matches_include("src/main.rs"));
        assert!(watcher.matches_include("Cargo.toml"));
        assert!(!watcher.matches_include("README.md"));
    }

    #[test]
    fn initial_scan_creates_snapshots() {
        let dir = make_temp_dir();
        fs::create_dir_all(dir.join("src")).expect("create src dir");
        fs::write(dir.join("src").join("main.rs"), "fn main() {}\n").expect("write file");

        let mut watcher = FileWatcher::new(dir.clone(), test_config());
        let count = watcher.initial_scan();

        assert_eq!(count, 1);
        assert_eq!(watcher.tracked_file_count(), 1);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn poll_detects_new_file() {
        let dir = make_temp_dir();
        fs::create_dir_all(dir.join("src")).expect("create src dir");

        let mut watcher = FileWatcher::new(dir.clone(), test_config());
        watcher.initial_scan();

        let file_path = dir.join("src").join("new.rs");
        fs::write(&file_path, "pub fn x() {}\n").expect("write new file");

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, file_path);
        assert_eq!(events[0].kind, ChangeKind::Created);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn poll_detects_modified_file() {
        let dir = make_temp_dir();
        fs::create_dir_all(dir.join("src")).expect("create src dir");
        let file_path = dir.join("src").join("mod.rs");
        fs::write(&file_path, "pub fn a() {}\n").expect("write file");

        let mut watcher = FileWatcher::new(dir.clone(), test_config());
        watcher.initial_scan();

        thread::sleep(Duration::from_millis(20));
        fs::write(&file_path, "pub fn a() { println!(\"x\"); }\n").expect("modify file");

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, file_path);
        assert_eq!(events[0].kind, ChangeKind::Modified);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn poll_detects_deleted_file() {
        let dir = make_temp_dir();
        fs::create_dir_all(dir.join("src")).expect("create src dir");
        let file_path = dir.join("src").join("gone.rs");
        fs::write(&file_path, "pub fn gone() {}\n").expect("write file");

        let mut watcher = FileWatcher::new(dir.clone(), test_config());
        watcher.initial_scan();

        fs::remove_file(&file_path).expect("remove file");

        let events = watcher.poll();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].path, file_path);
        assert_eq!(events[0].kind, ChangeKind::Deleted);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn change_kind_display() {
        assert_eq!(ChangeKind::Created.to_string(), "created");
        assert_eq!(ChangeKind::Modified.to_string(), "modified");
        assert_eq!(ChangeKind::Deleted.to_string(), "deleted");
    }

    #[test]
    fn status_summary_format() {
        let dir = make_temp_dir();
        let watcher = FileWatcher::new(dir.clone(), test_config());
        let summary = watcher.status_summary();

        assert!(summary.contains("Watching 0 files"));
        assert!(summary.contains("debounce: 0ms"));
        assert!(summary.contains("buffer:"));

        let _ = fs::remove_dir_all(dir);
    }
}

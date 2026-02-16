use chrono::{DateTime, Utc};
use orch_core::types::TaskId;
use std::collections::{HashMap, VecDeque};

const DEFAULT_MAX_LINES_PER_TASK: usize = 1_000;
const DEFAULT_MAX_TOTAL_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStreamConfig {
    pub max_lines_per_task: usize,
    pub max_total_bytes: usize,
}

impl Default for OutputStreamConfig {
    fn default() -> Self {
        Self {
            max_lines_per_task: DEFAULT_MAX_LINES_PER_TASK,
            max_total_bytes: DEFAULT_MAX_TOTAL_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputLine {
    pub task_id: TaskId,
    pub line: String,
    pub timestamp: DateTime<Utc>,
    pub source: OutputSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputSource {
    Stdout,
    Stderr,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputSnapshot {
    pub lines: Vec<OutputLine>,
    pub total_lines: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputStreamStats {
    pub total_lines: usize,
    pub total_bytes: usize,
    pub task_count: usize,
}

#[derive(Debug, Clone)]
pub struct OutputStreamManager {
    streams: HashMap<TaskId, TaskOutputStream>,
    config: OutputStreamConfig,
}

#[derive(Debug, Clone)]
struct TaskOutputStream {
    lines: VecDeque<OutputLine>,
    total_bytes: usize,
}

impl TaskOutputStream {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            total_bytes: 0,
        }
    }

    fn push_back(&mut self, line: OutputLine) {
        self.total_bytes += line.line.len();
        self.lines.push_back(line);
    }

    fn pop_front(&mut self) -> Option<OutputLine> {
        let line = self.lines.pop_front()?;
        self.total_bytes = self.total_bytes.saturating_sub(line.line.len());
        Some(line)
    }
}

impl OutputStreamManager {
    pub fn new(config: OutputStreamConfig) -> Self {
        Self {
            streams: HashMap::new(),
            config,
        }
    }

    pub fn append(&mut self, task_id: &TaskId, line: String, source: OutputSource) {
        let output_line = OutputLine {
            task_id: task_id.clone(),
            line,
            timestamp: Utc::now(),
            source,
        };

        self.streams
            .entry(task_id.clone())
            .or_insert_with(TaskOutputStream::new)
            .push_back(output_line);

        self.trim_task_lines(task_id);
        self.trim_total_bytes();
    }

    pub fn tail(&self, task_id: &TaskId, max_lines: usize) -> OutputSnapshot {
        let Some(stream) = self.streams.get(task_id) else {
            return OutputSnapshot {
                lines: Vec::new(),
                total_lines: 0,
                truncated: false,
            };
        };

        let total_lines = stream.lines.len();
        let lines = if max_lines == 0 {
            Vec::new()
        } else {
            stream
                .lines
                .iter()
                .skip(total_lines.saturating_sub(max_lines))
                .cloned()
                .collect()
        };

        OutputSnapshot {
            lines,
            total_lines,
            truncated: max_lines < total_lines,
        }
    }

    pub fn since(&self, task_id: &TaskId, after: DateTime<Utc>) -> OutputSnapshot {
        let Some(stream) = self.streams.get(task_id) else {
            return OutputSnapshot {
                lines: Vec::new(),
                total_lines: 0,
                truncated: false,
            };
        };

        let lines: Vec<OutputLine> = stream
            .lines
            .iter()
            .filter(|line| line.timestamp > after)
            .cloned()
            .collect();

        OutputSnapshot {
            total_lines: lines.len(),
            lines,
            truncated: false,
        }
    }

    pub fn clear(&mut self, task_id: &TaskId) {
        let _ = self.streams.remove(task_id);
    }

    pub fn clear_all(&mut self) {
        self.streams.clear();
    }

    pub fn active_tasks(&self) -> Vec<TaskId> {
        let mut task_ids: Vec<TaskId> = self.streams.keys().cloned().collect();
        task_ids.sort_by(|a, b| a.0.cmp(&b.0));
        task_ids
    }

    pub fn stats(&self) -> OutputStreamStats {
        let total_lines = self
            .streams
            .values()
            .map(|stream| stream.lines.len())
            .sum::<usize>();
        let total_bytes = self
            .streams
            .values()
            .map(|stream| stream.total_bytes)
            .sum::<usize>();

        OutputStreamStats {
            total_lines,
            total_bytes,
            task_count: self.streams.len(),
        }
    }

    pub fn search<'a>(&'a self, task_id: &TaskId, pattern: &str) -> Vec<&'a OutputLine> {
        self.streams
            .get(task_id)
            .map(|stream| {
                stream
                    .lines
                    .iter()
                    .filter(|line| line.line.contains(pattern))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn trim_task_lines(&mut self, task_id: &TaskId) {
        let Some(stream) = self.streams.get_mut(task_id) else {
            return;
        };

        while stream.lines.len() > self.config.max_lines_per_task {
            let _ = stream.pop_front();
        }

        if stream.lines.is_empty() {
            let _ = self.streams.remove(task_id);
        }
    }

    fn trim_total_bytes(&mut self) {
        while self.stats().total_bytes > self.config.max_total_bytes {
            if !self.pop_oldest_global_line() {
                break;
            }
        }
    }

    fn pop_oldest_global_line(&mut self) -> bool {
        let oldest_task_id = self
            .streams
            .iter()
            .filter_map(|(task_id, stream)| {
                stream
                    .lines
                    .front()
                    .map(|line| (task_id.clone(), line.timestamp))
            })
            .min_by(|(task_a, ts_a), (task_b, ts_b)| ts_a.cmp(ts_b).then_with(|| task_a.0.cmp(&task_b.0)))
            .map(|(task_id, _)| task_id);

        let Some(task_id) = oldest_task_id else {
            return false;
        };

        let remove_stream = {
            let Some(stream) = self.streams.get_mut(&task_id) else {
                return false;
            };
            let _ = stream.pop_front();
            stream.lines.is_empty()
        };

        if remove_stream {
            let _ = self.streams.remove(&task_id);
        }

        true
    }
}

impl Default for OutputStreamManager {
    fn default() -> Self {
        Self::new(OutputStreamConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn task_id(id: &str) -> TaskId {
        TaskId::new(id)
    }

    fn manager_with_limits(max_lines: usize, max_bytes: usize) -> OutputStreamManager {
        OutputStreamManager::new(OutputStreamConfig {
            max_lines_per_task: max_lines,
            max_total_bytes: max_bytes,
        })
    }

    #[test]
    fn output_stream_config_defaults() {
        let config = OutputStreamConfig::default();
        assert_eq!(config.max_lines_per_task, 1_000);
        assert_eq!(config.max_total_bytes, 10 * 1024 * 1024);
    }

    #[test]
    fn new_manager_starts_empty() {
        let manager = OutputStreamManager::default();
        assert!(manager.streams.is_empty());
        assert_eq!(manager.stats().total_lines, 0);
        assert_eq!(manager.stats().total_bytes, 0);
    }

    #[test]
    fn append_adds_line_for_task() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T1");

        manager.append(&id, "hello".to_string(), OutputSource::Stdout);

        let snapshot = manager.tail(&id, 10);
        assert_eq!(snapshot.total_lines, 1);
        assert_eq!(snapshot.lines[0].line, "hello");
        assert_eq!(snapshot.lines[0].task_id, id);
    }

    #[test]
    fn append_preserves_output_source() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T1");

        manager.append(&id, "warn".to_string(), OutputSource::Stderr);

        let snapshot = manager.tail(&id, 1);
        assert_eq!(snapshot.lines[0].source, OutputSource::Stderr);
    }

    #[test]
    fn tail_returns_last_n_lines() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T2");

        manager.append(&id, "l1".to_string(), OutputSource::Stdout);
        manager.append(&id, "l2".to_string(), OutputSource::Stdout);
        manager.append(&id, "l3".to_string(), OutputSource::Stdout);

        let snapshot = manager.tail(&id, 2);
        assert_eq!(snapshot.total_lines, 3);
        assert!(snapshot.truncated);
        assert_eq!(snapshot.lines.len(), 2);
        assert_eq!(snapshot.lines[0].line, "l2");
        assert_eq!(snapshot.lines[1].line, "l3");
    }

    #[test]
    fn tail_with_zero_max_lines_returns_empty_snapshot() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T3");

        manager.append(&id, "line".to_string(), OutputSource::Stdout);
        let snapshot = manager.tail(&id, 0);

        assert!(snapshot.lines.is_empty());
        assert_eq!(snapshot.total_lines, 1);
        assert!(snapshot.truncated);
    }

    #[test]
    fn tail_for_missing_task_is_empty() {
        let manager = OutputStreamManager::default();
        let snapshot = manager.tail(&task_id("missing"), 10);
        assert!(snapshot.lines.is_empty());
        assert_eq!(snapshot.total_lines, 0);
        assert!(!snapshot.truncated);
    }

    #[test]
    fn since_returns_only_lines_after_timestamp() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T4");
        let base = Utc::now();

        manager.append(&id, "first".to_string(), OutputSource::Stdout);
        manager.append(&id, "second".to_string(), OutputSource::Stdout);
        manager.append(&id, "third".to_string(), OutputSource::Stdout);

        let stream = manager.streams.get_mut(&id).expect("stream exists");
        stream.lines[0].timestamp = base;
        stream.lines[1].timestamp = base + Duration::seconds(1);
        stream.lines[2].timestamp = base + Duration::seconds(2);

        let snapshot = manager.since(&id, base);
        assert_eq!(snapshot.total_lines, 2);
        assert_eq!(snapshot.lines[0].line, "second");
        assert_eq!(snapshot.lines[1].line, "third");
        assert!(!snapshot.truncated);
    }

    #[test]
    fn since_for_missing_task_returns_empty() {
        let manager = OutputStreamManager::default();
        let snapshot = manager.since(&task_id("missing"), Utc::now());
        assert!(snapshot.lines.is_empty());
        assert_eq!(snapshot.total_lines, 0);
        assert!(!snapshot.truncated);
    }

    #[test]
    fn clear_removes_single_task_stream() {
        let mut manager = OutputStreamManager::default();
        let id1 = task_id("T5");
        let id2 = task_id("T6");

        manager.append(&id1, "a".to_string(), OutputSource::Stdout);
        manager.append(&id2, "b".to_string(), OutputSource::Stdout);
        manager.clear(&id1);

        assert!(!manager.streams.contains_key(&id1));
        assert!(manager.streams.contains_key(&id2));
    }

    #[test]
    fn clear_all_removes_all_streams() {
        let mut manager = OutputStreamManager::default();
        manager.append(&task_id("T7"), "a".to_string(), OutputSource::Stdout);
        manager.append(&task_id("T8"), "b".to_string(), OutputSource::Stdout);

        manager.clear_all();

        assert!(manager.streams.is_empty());
        assert_eq!(manager.stats().task_count, 0);
    }

    #[test]
    fn append_trims_to_max_lines_per_task() {
        let mut manager = manager_with_limits(2, 10_000);
        let id = task_id("T9");

        manager.append(&id, "one".to_string(), OutputSource::Stdout);
        manager.append(&id, "two".to_string(), OutputSource::Stdout);
        manager.append(&id, "three".to_string(), OutputSource::Stdout);

        let snapshot = manager.tail(&id, 10);
        assert_eq!(snapshot.total_lines, 2);
        assert_eq!(snapshot.lines[0].line, "two");
        assert_eq!(snapshot.lines[1].line, "three");
    }

    #[test]
    fn append_trims_global_oldest_lines_when_over_byte_limit() {
        let mut manager = manager_with_limits(10, 10);
        let t1 = task_id("A");
        let t2 = task_id("B");

        manager.append(&t1, "12345".to_string(), OutputSource::Stdout);
        manager.append(&t2, "67890".to_string(), OutputSource::Stdout);
        manager.append(&t1, "zz".to_string(), OutputSource::Stdout);

        let stats = manager.stats();
        assert_eq!(stats.total_bytes, 7);
        assert_eq!(manager.tail(&t1, 10).lines.len(), 1);
        assert_eq!(manager.tail(&t1, 10).lines[0].line, "zz");
        assert_eq!(manager.tail(&t2, 10).lines[0].line, "67890");
    }

    #[test]
    fn stats_aggregate_lines_bytes_and_tasks() {
        let mut manager = OutputStreamManager::default();
        let t1 = task_id("T10");
        let t2 = task_id("T11");

        manager.append(&t1, "abc".to_string(), OutputSource::Stdout);
        manager.append(&t1, "d".to_string(), OutputSource::Stdout);
        manager.append(&t2, "ef".to_string(), OutputSource::Stdout);

        let stats = manager.stats();
        assert_eq!(stats.total_lines, 3);
        assert_eq!(stats.total_bytes, 6);
        assert_eq!(stats.task_count, 2);
    }

    #[test]
    fn search_returns_matching_lines_for_task() {
        let mut manager = OutputStreamManager::default();
        let id = task_id("T12");

        manager.append(&id, "build started".to_string(), OutputSource::System);
        manager.append(&id, "build failed".to_string(), OutputSource::Stderr);
        manager.append(&id, "retry scheduled".to_string(), OutputSource::System);

        let matches = manager.search(&id, "build");
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().all(|line| line.line.contains("build")));
    }

    #[test]
    fn search_for_missing_task_returns_empty() {
        let manager = OutputStreamManager::default();
        let matches = manager.search(&task_id("missing"), "x");
        assert!(matches.is_empty());
    }

    #[test]
    fn active_tasks_returns_sorted_task_ids_with_output() {
        let mut manager = OutputStreamManager::default();
        manager.append(&task_id("T3"), "a".to_string(), OutputSource::Stdout);
        manager.append(&task_id("T1"), "b".to_string(), OutputSource::Stdout);
        manager.append(&task_id("T2"), "c".to_string(), OutputSource::Stdout);

        let active = manager.active_tasks();
        let values: Vec<String> = active.into_iter().map(|id| id.0).collect();
        assert_eq!(values, vec!["T1", "T2", "T3"]);
    }

    #[test]
    fn multiple_tasks_are_isolated_for_tail_and_search() {
        let mut manager = OutputStreamManager::default();
        let a = task_id("TA");
        let b = task_id("TB");

        manager.append(&a, "alpha one".to_string(), OutputSource::Stdout);
        manager.append(&b, "beta one".to_string(), OutputSource::Stdout);
        manager.append(&a, "alpha two".to_string(), OutputSource::Stdout);

        let tail_a = manager.tail(&a, 10);
        let tail_b = manager.tail(&b, 10);
        assert_eq!(tail_a.total_lines, 2);
        assert_eq!(tail_b.total_lines, 1);
        assert_eq!(manager.search(&a, "beta").len(), 0);
        assert_eq!(manager.search(&b, "beta").len(), 1);
    }
}

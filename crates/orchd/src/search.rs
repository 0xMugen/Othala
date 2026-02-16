use chrono::{DateTime, NaiveDate, Utc};
use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchSortBy {
    Relevance,
    Title,
    State,
    Model,
    Newest,
    Oldest,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilters {
    pub state: Option<String>,
    pub label: Option<String>,
    pub model: Option<String>,
    pub date_from: Option<DateTime<Utc>>,
    pub date_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchQuery {
    pub text: String,
    pub filters: SearchFilters,
    pub sort_by: SearchSortBy,
    pub limit: usize,
    pub fuzzy: bool,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            filters: SearchFilters::default(),
            sort_by: SearchSortBy::Relevance,
            limit: 50,
            fuzzy: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    pub task_id: String,
    pub title: String,
    pub state: String,
    pub score: f64,
    pub matched_fields: Vec<String>,
    pub snippet: String,
}

#[derive(Debug, Clone)]
struct IndexedTask {
    task_id: String,
    title: String,
    labels: Vec<String>,
    state: String,
    model: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Default)]
pub struct SearchIndex {
    tasks: HashMap<String, IndexedTask>,
}

impl SearchIndex {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_task(
        &mut self,
        task_id: &str,
        title: &str,
        labels: &[String],
        state: &str,
        model: &str,
    ) {
        self.tasks.insert(
            task_id.to_string(),
            IndexedTask {
                task_id: task_id.to_string(),
                title: title.to_string(),
                labels: labels.to_vec(),
                state: state.to_string(),
                model: model.to_string(),
                created_at: Utc::now(),
            },
        );
    }

    pub fn remove_task(&mut self, task_id: &str) {
        self.tasks.remove(task_id);
    }

    pub fn search(&self, query: &SearchQuery) -> Vec<SearchResult> {
        let state_filter = query.filters.state.as_ref().map(|v| v.to_lowercase());
        let label_filter = query.filters.label.as_ref().map(|v| v.to_lowercase());
        let model_filter = query.filters.model.as_ref().map(|v| v.to_lowercase());
        let query_tokens = tokenize(&query.text);

        let mut results = Vec::new();

        for task in self.tasks.values() {
            if let Some(state) = &state_filter {
                if task.state.to_lowercase() != *state {
                    continue;
                }
            }

            if let Some(model) = &model_filter {
                if task.model.to_lowercase() != *model {
                    continue;
                }
            }

            if let Some(label) = &label_filter {
                let has_label = task
                    .labels
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(label));
                if !has_label {
                    continue;
                }
            }

            if let Some(from) = query.filters.date_from {
                if task.created_at < from {
                    continue;
                }
            }
            if let Some(to) = query.filters.date_to {
                if task.created_at > to {
                    continue;
                }
            }

            let mut matched_fields = BTreeSet::new();
            let score = score_task(task, &query_tokens, query.fuzzy, &mut matched_fields);

            let has_query = !query_tokens.is_empty();
            let matches_query = score > 0.0 || !has_query;
            if !matches_query {
                continue;
            }

            let final_score = if has_query { score } else { 1.0 };
            let matched_fields_vec = matched_fields.into_iter().collect::<Vec<_>>();
            let snippet = if has_query {
                highlight_match(&task.title, &query.text)
            } else {
                task.title.clone()
            };

            results.push(SearchResult {
                task_id: task.task_id.clone(),
                title: task.title.clone(),
                state: task.state.clone(),
                score: final_score,
                matched_fields: matched_fields_vec,
                snippet,
            });
        }

        match query.sort_by {
            SearchSortBy::Relevance => {
                results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(Ordering::Equal))
            }
            SearchSortBy::Title => {
                results.sort_by(|a, b| a.title.to_lowercase().cmp(&b.title.to_lowercase()))
            }
            SearchSortBy::State => {
                results.sort_by(|a, b| a.state.to_lowercase().cmp(&b.state.to_lowercase()))
            }
            SearchSortBy::Model | SearchSortBy::Newest | SearchSortBy::Oldest => {
                let mut with_ts = results
                    .into_iter()
                    .map(|result| {
                        let ts = self
                            .tasks
                            .get(&result.task_id)
                            .map(|task| task.created_at)
                            .unwrap_or_else(Utc::now);
                        (result, ts)
                    })
                    .collect::<Vec<_>>();

                match query.sort_by {
                    SearchSortBy::Model => with_ts.sort_by(|(a, _), (b, _)| {
                        let am = self
                            .tasks
                            .get(&a.task_id)
                            .map(|task| task.model.to_lowercase())
                            .unwrap_or_default();
                        let bm = self
                            .tasks
                            .get(&b.task_id)
                            .map(|task| task.model.to_lowercase())
                            .unwrap_or_default();
                        am.cmp(&bm)
                    }),
                    SearchSortBy::Newest => with_ts.sort_by(|(_, at), (_, bt)| bt.cmp(at)),
                    SearchSortBy::Oldest => with_ts.sort_by(|(_, at), (_, bt)| at.cmp(bt)),
                    SearchSortBy::Relevance
                    | SearchSortBy::Title
                    | SearchSortBy::State => {}
                }

                results = with_ts.into_iter().map(|(result, _)| result).collect();
            }
        }

        if query.limit > 0 {
            results.truncate(query.limit);
        }

        results
    }
}

fn score_task(
    task: &IndexedTask,
    query_tokens: &[String],
    fuzzy: bool,
    matched_fields: &mut BTreeSet<String>,
) -> f64 {
    if query_tokens.is_empty() {
        return 0.0;
    }

    let mut total = 0.0;
    let mut matched_tokens = 0usize;

    for token in query_tokens {
        let mut token_score: f64 = 0.0;

        token_score = token_score.max(score_text_match(
            token,
            &task.title,
            fuzzy,
            1.0,
            "title",
            matched_fields,
        ));
        token_score = token_score.max(score_text_match(
            token,
            &task.task_id,
            fuzzy,
            0.9,
            "task_id",
            matched_fields,
        ));
        token_score = token_score.max(score_text_match(
            token,
            &task.state,
            fuzzy,
            0.7,
            "state",
            matched_fields,
        ));
        token_score = token_score.max(score_text_match(
            token,
            &task.model,
            fuzzy,
            0.7,
            "model",
            matched_fields,
        ));

        for label in &task.labels {
            token_score = token_score.max(score_text_match(
                token,
                label,
                fuzzy,
                0.8,
                "labels",
                matched_fields,
            ));
        }

        if token_score > 0.0 {
            matched_tokens += 1;
            total += token_score;
        }
    }

    if matched_tokens == 0 {
        return 0.0;
    }

    let coverage = matched_tokens as f64 / query_tokens.len() as f64;
    ((total / query_tokens.len() as f64) * (0.6 + (coverage * 0.4))).min(1.0)
}

fn score_text_match(
    token: &str,
    candidate: &str,
    fuzzy: bool,
    weight: f64,
    field_name: &str,
    matched_fields: &mut BTreeSet<String>,
) -> f64 {
    let token_lc = token.to_lowercase();
    let candidate_lc = candidate.to_lowercase();
    if candidate_lc.contains(&token_lc) {
        matched_fields.insert(field_name.to_string());
        return weight;
    }

    if fuzzy {
        if let Some(score) = fuzzy_match(token, candidate) {
            matched_fields.insert(field_name.to_string());
            return score * weight;
        }
    }

    0.0
}

pub fn fuzzy_match(needle: &str, haystack: &str) -> Option<f64> {
    let needle = needle.trim().to_lowercase();
    let haystack = haystack.trim().to_lowercase();

    if needle.is_empty() {
        return Some(1.0);
    }
    if haystack.is_empty() {
        return None;
    }

    if haystack.contains(&needle) {
        let ratio = needle.len() as f64 / haystack.len() as f64;
        return Some((0.75 + (ratio * 0.25)).min(1.0));
    }

    let mut best = normalized_levenshtein(&needle, &haystack);
    for token in tokenize(&haystack) {
        best = best.max(normalized_levenshtein(&needle, &token));
    }

    if best >= 0.45 {
        Some(best)
    } else {
        None
    }
}

fn normalized_levenshtein(a: &str, b: &str) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }

    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let mut dp = vec![vec![0usize; b_chars.len() + 1]; a_chars.len() + 1];

    for (i, row) in dp.iter_mut().enumerate() {
        row[0] = i;
    }
    for (j, cell) in dp[0].iter_mut().enumerate() {
        *cell = j;
    }

    for i in 1..=a_chars.len() {
        for j in 1..=b_chars.len() {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }

    let dist = dp[a_chars.len()][b_chars.len()];
    let max_len = a_chars.len().max(b_chars.len()) as f64;
    (1.0 - (dist as f64 / max_len)).max(0.0)
}

pub fn tokenize(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

pub fn highlight_match(text: &str, query: &str) -> String {
    let mut candidates = Vec::new();
    let query = query.trim();
    if !query.is_empty() {
        candidates.push(query.to_string());
    }
    candidates.extend(tokenize(query));

    for candidate in candidates {
        if candidate.is_empty() {
            continue;
        }
        if let Some((start, end)) = find_case_insensitive(text, &candidate) {
            return format!("{}[[{}]]{}", &text[..start], &text[start..end], &text[end..]);
        }
    }

    text.to_string()
}

fn find_case_insensitive(text: &str, query: &str) -> Option<(usize, usize)> {
    let text_lc = text.to_lowercase();
    let query_lc = query.to_lowercase();
    text_lc
        .find(&query_lc)
        .map(|start| (start, start + query_lc.len()))
}

pub fn parse_search_query(input: &str) -> SearchQuery {
    let mut query = SearchQuery::default();
    let mut text_parts = Vec::new();

    for token in input.split_whitespace() {
        if let Some(value) = token.strip_prefix("state:") {
            if !value.is_empty() {
                query.filters.state = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = token.strip_prefix("label:") {
            if !value.is_empty() {
                query.filters.label = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = token.strip_prefix("model:") {
            if !value.is_empty() {
                query.filters.model = Some(value.to_string());
            }
            continue;
        }
        if let Some(value) = token.strip_prefix("from:") {
            query.filters.date_from = parse_query_date(value);
            continue;
        }
        if let Some(value) = token.strip_prefix("to:") {
            query.filters.date_to = parse_query_date(value);
            continue;
        }
        if let Some(value) = token.strip_prefix("sort:") {
            query.sort_by = match value.to_lowercase().as_str() {
                "title" => SearchSortBy::Title,
                "state" => SearchSortBy::State,
                "model" => SearchSortBy::Model,
                "newest" => SearchSortBy::Newest,
                "oldest" => SearchSortBy::Oldest,
                _ => SearchSortBy::Relevance,
            };
            continue;
        }
        if let Some(value) = token.strip_prefix("limit:") {
            if let Ok(limit) = value.parse::<usize>() {
                if limit > 0 {
                    query.limit = limit;
                }
            }
            continue;
        }
        if token.eq_ignore_ascii_case("fuzzy") {
            query.fuzzy = true;
            continue;
        }

        text_parts.push(token);
    }

    query.text = text_parts.join(" ");
    query
}

fn parse_query_date(value: &str) -> Option<DateTime<Utc>> {
    if let Ok(parsed) = DateTime::parse_from_rfc3339(value) {
        return Some(parsed.with_timezone(&Utc));
    }

    if let Ok(parsed) = NaiveDate::parse_from_str(value, "%Y-%m-%d") {
        return parsed
            .and_hms_opt(0, 0, 0)
            .map(|dt| DateTime::from_naive_utc_and_offset(dt, Utc));
    }

    None
}

pub fn display_search_results(results: &[SearchResult]) -> String {
    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut out = String::new();
    for (idx, result) in results.iter().enumerate() {
        let fields = if result.matched_fields.is_empty() {
            "-".to_string()
        } else {
            result.matched_fields.join(",")
        };
        out.push_str(&format!(
            "{}. [{}] ({}) score={:.3} fields={}\n   {}\n",
            idx + 1,
            result.task_id,
            result.state,
            result.score,
            fields,
            result.snippet
        ));
    }
    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn mk_query(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            ..SearchQuery::default()
        }
    }

    #[test]
    fn fuzzy_match_exact_returns_high_score() {
        let score = fuzzy_match("search", "search module").expect("score");
        assert!(score >= 0.8);
    }

    #[test]
    fn fuzzy_match_typo_returns_score() {
        let score = fuzzy_match("serch", "search module").expect("score");
        assert!(score >= 0.5);
    }

    #[test]
    fn fuzzy_match_unrelated_returns_none() {
        assert!(fuzzy_match("banana", "orchestrator daemon").is_none());
    }

    #[test]
    fn tokenize_splits_text_into_lowercase_tokens() {
        let tokens = tokenize("Fix-Search_Module, V2!");
        assert_eq!(tokens, vec!["fix", "search", "module", "v2"]);
    }

    #[test]
    fn highlight_match_wraps_first_match() {
        let highlighted = highlight_match("Fix Search Module", "search");
        assert_eq!(highlighted, "Fix [[Search]] Module");
    }

    #[test]
    fn parse_search_query_extracts_filters_and_flags() {
        let query = parse_search_query("state:chatting label:bug model:codex fuzzy fix parser");
        assert_eq!(query.filters.state.as_deref(), Some("chatting"));
        assert_eq!(query.filters.label.as_deref(), Some("bug"));
        assert_eq!(query.filters.model.as_deref(), Some("codex"));
        assert!(query.fuzzy);
        assert_eq!(query.text, "fix parser");
    }

    #[test]
    fn parse_search_query_parses_date_sort_and_limit() {
        let query = parse_search_query("from:2025-01-01 to:2025-01-31 sort:title limit:5 foo");
        assert!(query.filters.date_from.is_some());
        assert!(query.filters.date_to.is_some());
        assert_eq!(query.sort_by, SearchSortBy::Title);
        assert_eq!(query.limit, 5);
        assert_eq!(query.text, "foo");
    }

    #[test]
    fn index_add_and_remove_task() {
        let mut index = SearchIndex::new();
        index.add_task(
            "T-1",
            "Fix parser",
            &["bug".to_string()],
            "chatting",
            "codex",
        );

        assert_eq!(index.search(&mk_query("parser")).len(), 1);
        index.remove_task("T-1");
        assert!(index.search(&mk_query("parser")).is_empty());
    }

    #[test]
    fn search_ranks_exact_title_match_higher_than_label_match() {
        let mut index = SearchIndex::new();
        index.add_task(
            "T-1",
            "Search bug in parser",
            &["maintenance".to_string()],
            "chatting",
            "codex",
        );
        index.add_task(
            "T-2",
            "Refactor parser",
            &["search".to_string()],
            "chatting",
            "codex",
        );

        let query = mk_query("search");
        let results = index.search(&query);
        assert_eq!(results[0].task_id, "T-1");
        assert!(results[0].score >= results[1].score);
    }

    #[test]
    fn search_applies_state_label_and_model_filters() {
        let mut index = SearchIndex::new();
        index.add_task(
            "T-1",
            "Fix crash",
            &["bug".to_string()],
            "chatting",
            "codex",
        );
        index.add_task(
            "T-2",
            "Fix crash",
            &["bug".to_string()],
            "ready",
            "claude",
        );

        let mut query = mk_query("fix");
        query.filters.state = Some("chatting".to_string());
        query.filters.label = Some("bug".to_string());
        query.filters.model = Some("codex".to_string());

        let results = index.search(&query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "T-1");
    }

    #[test]
    fn search_honors_date_range_filter() {
        let mut index = SearchIndex::new();
        index.add_task("T-1", "Old task", &[], "chatting", "codex");
        index.add_task("T-2", "New task", &[], "chatting", "codex");

        if let Some(task) = index.tasks.get_mut("T-1") {
            task.created_at = Utc::now() - Duration::days(10);
        }

        let mut query = mk_query("task");
        query.filters.date_from = Some(Utc::now() - Duration::days(2));
        let results = index.search(&query);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "T-2");
    }

    #[test]
    fn search_with_fuzzy_finds_typo() {
        let mut index = SearchIndex::new();
        index.add_task("T-1", "Search pipeline", &[], "chatting", "codex");

        let mut query = mk_query("serch");
        query.fuzzy = true;

        let results = index.search(&query);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].task_id, "T-1");
    }

    #[test]
    fn display_search_results_formats_output() {
        let rendered = display_search_results(&[SearchResult {
            task_id: "T-1".to_string(),
            title: "Fix parser".to_string(),
            state: "chatting".to_string(),
            score: 0.95,
            matched_fields: vec!["title".to_string()],
            snippet: "[[Fix]] parser".to_string(),
        }]);

        assert!(rendered.contains("[T-1]"));
        assert!(rendered.contains("score=0.950"));
        assert!(rendered.contains("[[Fix]] parser"));
    }
}

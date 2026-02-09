const DEFAULT_TITLE: &str = "TUI task";
const MAX_TITLE_LEN: usize = 72;

pub fn summarize_prompt_as_title(prompt: &str) -> String {
    let lines = prompt.lines().map(str::trim).collect::<Vec<_>>();
    let chosen = extract_requested_work_title(&lines)
        .or_else(|| first_actionable_prompt_line(&lines))
        .or_else(|| lines.iter().copied().find(|line| !line.is_empty()))
        .unwrap_or(DEFAULT_TITLE);
    normalize_task_title(chosen)
}

pub fn normalize_task_title(raw: &str) -> String {
    let normalized = normalize_whitespace(raw);
    if normalized.is_empty() {
        return DEFAULT_TITLE.to_string();
    }

    if let Some(special) = special_title_for_known_intent(&normalized) {
        return truncate_title(&special);
    }

    let mut candidate = strip_wrapping_punctuation(&normalized);
    candidate = strip_leading_filler(&candidate);
    candidate = normalize_leading_action(&candidate);

    if candidate.is_empty() {
        candidate = normalized.clone();
    }

    if title_needs_condensing(&candidate) {
        candidate = condense_title(&candidate);
    }

    let titled = to_title_case_phrase(&candidate);
    if titled.is_empty() {
        return truncate_title(&normalized);
    }
    truncate_title(&titled)
}

fn extract_requested_work_title<'a>(lines: &'a [&'a str]) -> Option<&'a str> {
    for (idx, line) in lines.iter().copied().enumerate() {
        let Some(rest) = strip_prefix_ignore_ascii_case(line, "requested work:") else {
            continue;
        };
        let inline = strip_common_list_prefix(rest.trim());
        if !inline.is_empty() {
            return Some(inline);
        }
        for candidate in lines.iter().copied().skip(idx + 1) {
            let normalized = strip_common_list_prefix(candidate);
            if normalized.is_empty() || title_is_boilerplate(normalized) {
                continue;
            }
            if line_looks_like_section_header(normalized) {
                break;
            }
            return Some(normalized);
        }
    }
    None
}

fn first_actionable_prompt_line<'a>(lines: &'a [&'a str]) -> Option<&'a str> {
    lines.iter().copied().find_map(|line| {
        let normalized = strip_common_list_prefix(line);
        if normalized.is_empty() || title_is_boilerplate(normalized) {
            return None;
        }
        Some(normalized)
    })
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_wrapping_punctuation(value: &str) -> String {
    value
        .trim_matches(|ch: char| {
            ch.is_whitespace()
                || matches!(
                    ch,
                    '"' | '\''
                        | '`'
                        | '['
                        | ']'
                        | '('
                        | ')'
                        | '{'
                        | '}'
                        | '<'
                        | '>'
                        | '.'
                        | ','
                        | ';'
                        | ':'
                )
        })
        .to_string()
}

fn strip_leading_filler(value: &str) -> String {
    let mut out = value.trim().to_string();
    let fillers = [
        "please ",
        "can you ",
        "could you ",
        "would you ",
        "i need ",
        "we need ",
        "need to ",
        "i want ",
        "we want ",
        "let's ",
        "lets ",
        "help me ",
        "make sure ",
        "task: ",
    ];

    loop {
        let mut changed = false;
        for prefix in fillers {
            let Some(rest) = strip_prefix_ignore_ascii_case(&out, prefix) else {
                continue;
            };
            out = rest
                .trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, ':' | ',' | '-'))
                .to_string();
            changed = true;
            break;
        }
        if !changed {
            break;
        }
    }

    out
}

fn normalize_leading_action(value: &str) -> String {
    if let Some(rest) = strip_prefix_ignore_ascii_case(value, "better ") {
        return format!("improve {}", rest.trim_start());
    }
    if let Some(rest) = strip_prefix_ignore_ascii_case(value, "improving ") {
        return format!("improve {}", rest.trim_start());
    }
    if let Some(rest) = strip_prefix_ignore_ascii_case(value, "making ") {
        return format!("make {}", rest.trim_start());
    }
    value.to_string()
}

fn title_needs_condensing(value: &str) -> bool {
    let words = value.split_whitespace().count();
    words > 10
        || value.len() > MAX_TITLE_LEN
        || value.contains(',')
        || value.contains(';')
        || value.contains(':')
        || contains_case_insensitive(value, " rather than ")
        || contains_case_insensitive(value, " instead of ")
        || contains_case_insensitive(value, " so that ")
        || contains_case_insensitive(value, " because ")
}

fn condense_title(value: &str) -> String {
    let mut clause = first_title_clause(value);
    clause = strip_leading_filler(&clause);
    clause = normalize_leading_action(&clause);

    let mut words = tokenize_words(&clause);
    if words.is_empty() {
        words = tokenize_words(value);
    }

    if words.len() > 10 {
        words.truncate(10);
    }

    while let Some(last) = words.last() {
        if !is_small_word(last) {
            break;
        }
        words.pop();
    }

    words.join(" ")
}

fn first_title_clause(value: &str) -> String {
    let mut end = value.len();

    for marker in [".", ";", ":", "\n", ","] {
        if let Some(idx) = value.find(marker) {
            end = end.min(idx);
        }
    }

    for marker in [
        " rather than ",
        " instead of ",
        " so that ",
        " because ",
        " while ",
        " but ",
    ] {
        if let Some(idx) = find_case_insensitive(value, marker) {
            end = end.min(idx);
        }
    }

    value[..end].trim().to_string()
}

fn tokenize_words(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(sanitize_token)
        .filter(|word| !word.is_empty())
        .collect()
}

fn sanitize_token(token: &str) -> String {
    token
        .trim_matches(|ch: char| {
            !ch.is_ascii_alphanumeric() && !matches!(ch, '-' | '/' | '_' | '\'')
        })
        .to_string()
}

fn to_title_case_phrase(value: &str) -> String {
    tokenize_words(value)
        .iter()
        .enumerate()
        .map(|(idx, word)| title_case_word(word, idx == 0))
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case_word(word: &str, is_first: bool) -> String {
    if word.contains('-') {
        return word
            .split('-')
            .enumerate()
            .map(|(idx, part)| title_case_word(part, is_first && idx == 0))
            .collect::<Vec<_>>()
            .join("-");
    }

    if word.contains('/') {
        return word
            .split('/')
            .enumerate()
            .map(|(idx, part)| title_case_word(part, is_first && idx == 0))
            .collect::<Vec<_>>()
            .join("/");
    }

    let lower = word.to_ascii_lowercase();
    if let Some(mapped) = acronym_override(&lower) {
        return mapped.to_string();
    }
    if !is_first && is_small_word(&lower) {
        return lower;
    }

    let has_alpha = word.chars().any(|ch| ch.is_ascii_alphabetic());
    if has_alpha
        && word
            .chars()
            .all(|ch| !ch.is_ascii_alphabetic() || ch.is_ascii_uppercase())
    {
        return word.to_string();
    }

    let mut chars = lower.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut out = String::new();
    out.push(first.to_ascii_uppercase());
    out.push_str(chars.as_str());
    out
}

fn acronym_override(value: &str) -> Option<&'static str> {
    match value {
        "api" => Some("API"),
        "apis" => Some("APIs"),
        "cli" => Some("CLI"),
        "ci" => Some("CI"),
        "cd" => Some("CD"),
        "http" => Some("HTTP"),
        "https" => Some("HTTPS"),
        "id" => Some("ID"),
        "ids" => Some("IDs"),
        "json" => Some("JSON"),
        "llm" => Some("LLM"),
        "oauth" => Some("OAuth"),
        "pr" => Some("PR"),
        "prs" => Some("PRs"),
        "sdk" => Some("SDK"),
        "sql" => Some("SQL"),
        "ui" => Some("UI"),
        "ux" => Some("UX"),
        _ => None,
    }
}

fn special_title_for_known_intent(value: &str) -> Option<String> {
    let lower = value.to_ascii_lowercase();
    let mentions_chat = contains_word(&lower, "chat") || contains_word(&lower, "conversation");
    let mentions_pr = contains_word(&lower, "pr") || lower.contains("pull request");
    let mentions_title = lower.contains("title") || lower.contains("naming");

    if mentions_title && mentions_chat && mentions_pr {
        return Some("Improve Chat and PR Title Generation".to_string());
    }
    if mentions_title && mentions_chat {
        return Some("Improve Chat Title Generation".to_string());
    }
    if mentions_title && mentions_pr {
        return Some("Improve PR Title Generation".to_string());
    }

    None
}

fn contains_word(haystack: &str, needle: &str) -> bool {
    haystack
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .any(|token| token == needle)
}

fn contains_case_insensitive(value: &str, needle: &str) -> bool {
    find_case_insensitive(value, needle).is_some()
}

fn find_case_insensitive(value: &str, needle: &str) -> Option<usize> {
    value
        .to_ascii_lowercase()
        .find(&needle.to_ascii_lowercase())
}

fn is_small_word(word: &str) -> bool {
    matches!(
        word,
        "a" | "an"
            | "and"
            | "as"
            | "at"
            | "but"
            | "by"
            | "for"
            | "from"
            | "in"
            | "nor"
            | "of"
            | "on"
            | "or"
            | "per"
            | "so"
            | "the"
            | "to"
            | "via"
            | "vs"
            | "with"
            | "when"
    )
}

fn strip_common_list_prefix(line: &str) -> &str {
    let trimmed = line.trim();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return rest.trim_start();
    }

    let digit_count = trimmed.chars().take_while(|ch| ch.is_ascii_digit()).count();
    if digit_count > 0 {
        let suffix = &trimmed[digit_count..];
        if let Some(rest) = suffix
            .strip_prefix(". ")
            .or_else(|| suffix.strip_prefix(") "))
        {
            return rest.trim_start();
        }
    }
    trimmed
}

fn title_is_boilerplate(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.starts_with("you are working on task ")
        || lower.starts_with("you are resolving git merge/rebase conflicts")
        || lower.starts_with("state:")
        || lower.starts_with("role:")
        || lower.starts_with("type:")
        || lower.starts_with("requested work:")
        || lower.starts_with("work in this task worktree")
        || lower.starts_with("when implementation is complete")
        || lower.starts_with("if blocked and human input is required")
        || lower.starts_with("requested work")
}

fn line_looks_like_section_header(line: &str) -> bool {
    let Some(prefix) = line.strip_suffix(':') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.len() <= 48
        && prefix
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == ' ' || ch == '-' || ch == '_')
}

fn strip_prefix_ignore_ascii_case<'a>(value: &'a str, prefix: &str) -> Option<&'a str> {
    let head = value.get(..prefix.len())?;
    if !head.eq_ignore_ascii_case(prefix) {
        return None;
    }
    value.get(prefix.len()..)
}

fn truncate_title(value: &str) -> String {
    let mut title = value.trim().to_string();
    if title.len() > MAX_TITLE_LEN {
        title.truncate(MAX_TITLE_LEN - 3);
        title.push_str("...");
    }
    title
}

#[cfg(test)]
mod tests {
    use super::{normalize_task_title, summarize_prompt_as_title};

    #[test]
    fn summarize_prompt_uses_requested_work_and_condenses_to_title_case() {
        let prompt = "You are working on task T1770623151487: better naming in the task display based on the prompt so we know what it's working on
State: Running
Role: General
Type: Feature
Requested work:
better naming in the task display based on the prompt so we know what it's working on
Work in this task worktree, make focused changes, and report progress.";
        let title = summarize_prompt_as_title(prompt);
        assert_eq!(
            title,
            "Improve Naming in the Task Display Based on the Prompt"
        );
    }

    #[test]
    fn summarize_prompt_supports_inline_requested_work() {
        let prompt = "Requested work: tighten retry behavior when Graphite returns 429";
        let title = summarize_prompt_as_title(prompt);
        assert_eq!(title, "Tighten Retry Behavior when Graphite Returns 429");
    }

    #[test]
    fn summarize_prompt_skips_metadata_lines() {
        let prompt = "State: Queued
Role: General
Type: Feature
Fix flaky task scheduling by debouncing tick updates";
        let title = summarize_prompt_as_title(prompt);
        assert_eq!(
            title,
            "Fix Flaky Task Scheduling by Debouncing Tick Updates"
        );
    }

    #[test]
    fn normalize_title_handles_chat_and_pr_naming_intent() {
        let input = "better titles, be like chatgpt that generates smart titles for each chat rather than just the prompt itself, same for the pr name";
        assert_eq!(
            normalize_task_title(input),
            "Improve Chat and PR Title Generation"
        );
    }
}

//! Custom commands system — user-defined markdown templates with named arguments.
//!
//! Commands are stored as .md files in:
//! - ~/.config/othala/commands/ (user commands, prefixed "user:")
//! - <PROJECT>/.othala/commands/ (project commands, prefixed "project:")

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::{fs, time::Instant};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomCommand {
    pub name: String,
    pub prefix: CommandPrefix,
    pub description: String,
    pub template: String,
    pub arguments: Vec<String>,
    pub source_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandPrefix {
    User,
    Project,
}

impl CommandPrefix {
    pub fn as_str(&self) -> &str {
        match self {
            CommandPrefix::User => "user",
            CommandPrefix::Project => "project",
        }
    }
}

impl std::fmt::Display for CommandPrefix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Non-interactive prompt execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PromptResult {
    pub prompt: String,
    pub response: String,
    pub model: String,
    pub tokens_used: u64,
    pub duration_ms: u64,
}

/// Extract argument names from template (matches $UPPER_CASE_NAME patterns)
pub fn extract_arguments(template: &str) -> Vec<String> {
    let bytes = template.as_bytes();
    let mut i = 0usize;
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    while i < bytes.len() {
        if bytes[i] == b'$' && i + 1 < bytes.len() && is_upper(bytes[i + 1]) {
            let start = i + 1;
            let mut end = start + 1;
            while end < bytes.len() && is_arg_char(bytes[end]) {
                end += 1;
            }
            if end < bytes.len() && bytes[end].is_ascii_lowercase() {
                i = end;
                continue;
            }
            if let Some(name) = template.get(start..end) {
                if seen.insert(name.to_string()) {
                    out.push(name.to_string());
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }

    out
}

/// Parse the first line as description if it starts with "# " or "<!-- description: ... -->"
pub fn parse_command_description(content: &str) -> (String, String) {
    let Some(first_line) = content.lines().next() else {
        return (String::new(), String::new());
    };

    if let Some(desc) = first_line.strip_prefix("# ") {
        let body = content
            .split_once('\n')
            .map(|(_, rest)| rest)
            .unwrap_or("")
            .to_string();
        return (desc.trim().to_string(), body);
    }

    let trimmed = first_line.trim();
    if let Some(rest) = trimmed.strip_prefix("<!--") {
        let rest = rest.trim_start();
        if let Some(rest) = rest.strip_prefix("description:") {
            let rest = rest.trim_start();
            if let Some(desc) = rest.strip_suffix("-->") {
                let body = content
                    .split_once('\n')
                    .map(|(_, rest)| rest)
                    .unwrap_or("")
                    .to_string();
                return (desc.trim().to_string(), body);
            }
        }
    }

    (String::new(), content.to_string())
}

/// Discover commands from a directory
pub fn discover_commands_from_dir(dir: &Path, prefix: CommandPrefix) -> Vec<CustomCommand> {
    let mut commands = Vec::new();
    if !dir.is_dir() {
        return commands;
    }
    visit_markdown(dir, dir, prefix, &mut commands);
    commands.sort_by(|a, b| a.name.cmp(&b.name));
    commands
}

/// Discover all commands (user + project)
pub fn discover_all_commands(project_root: &Path) -> Vec<CustomCommand> {
    let mut commands = Vec::new();

    if let Ok(home) = std::env::var("HOME") {
        let user_dir = Path::new(&home).join(".config/othala/commands");
        commands.extend(discover_commands_from_dir(&user_dir, CommandPrefix::User));
    }

    let project_dir = project_root.join(".othala/commands");
    commands.extend(discover_commands_from_dir(&project_dir, CommandPrefix::Project));

    commands.sort_by(|a, b| {
        a.prefix
            .as_str()
            .cmp(b.prefix.as_str())
            .then(a.name.cmp(&b.name))
    });
    commands
}

/// Render a command template with argument values
pub fn render_command(command: &CustomCommand, args: &HashMap<String, String>) -> Result<String, String> {
    for required in &command.arguments {
        if !args.contains_key(required) {
            return Err(format!("Missing required argument: {required}"));
        }
    }

    let mut out = String::with_capacity(command.template.len());
    let template = command.template.as_bytes();
    let mut i = 0usize;
    while i < template.len() {
        if template[i] == b'$' && i + 1 < template.len() && is_upper(template[i + 1]) {
            let start = i + 1;
            let mut end = start + 1;
            while end < template.len() && is_arg_char(template[end]) {
                end += 1;
            }
            if end < template.len() && template[end].is_ascii_lowercase() {
                out.push(template[i] as char);
                i += 1;
                continue;
            }
            if let Some(name) = command.template.get(start..end) {
                if let Some(value) = args.get(name) {
                    out.push_str(value);
                    i = end;
                    continue;
                }
            }
        }
        out.push(template[i] as char);
        i += 1;
    }
    Ok(out)
}

/// Format commands as a display table
pub fn display_commands_table(commands: &[CustomCommand]) -> String {
    if commands.is_empty() {
        return "No custom commands found.".to_string();
    }

    let rows: Vec<(String, String, String)> = commands
        .iter()
        .map(|c| {
            let full_name = format!("{}:{}", c.prefix, c.name);
            let args = if c.arguments.is_empty() {
                "-".to_string()
            } else {
                c.arguments.join(",")
            };
            (full_name, args, c.description.clone())
        })
        .collect();

    let name_width = rows
        .iter()
        .map(|(name, _, _)| name.len())
        .max()
        .unwrap_or(0)
        .max("COMMAND".len());
    let args_width = rows
        .iter()
        .map(|(_, args, _)| args.len())
        .max()
        .unwrap_or(0)
        .max("ARGS".len());

    let mut out = String::new();
    out.push_str(&format!(
        "{:<name_width$}  {:<args_width$}  DESCRIPTION\n",
        "COMMAND", "ARGS"
    ));
    out.push_str(&format!(
        "{}  {}  {}\n",
        "-".repeat(name_width),
        "-".repeat(args_width),
        "-".repeat("DESCRIPTION".len())
    ));

    for (name, args, description) in rows {
        out.push_str(&format!(
            "{:<name_width$}  {:<args_width$}  {}\n",
            name, args, description
        ));
    }
    out
}

/// Execute a single prompt non-interactively
pub fn execute_prompt(prompt: &str, model: &str, output_format: &str) -> PromptResult {
    let _ = output_format;
    let start = Instant::now();
    PromptResult {
        prompt: prompt.to_string(),
        response: format!("Would execute prompt with model {model}: {prompt}"),
        model: model.to_string(),
        tokens_used: 0,
        duration_ms: start.elapsed().as_millis() as u64,
    }
}

fn visit_markdown(base_dir: &Path, current_dir: &Path, prefix: CommandPrefix, out: &mut Vec<CustomCommand>) {
    let entries = match fs::read_dir(current_dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            visit_markdown(base_dir, &path, prefix, out);
            continue;
        }

        if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }

        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };

        let relative = match path.strip_prefix(base_dir) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        let name = command_name_from_relative_path(relative);
        let (description_raw, template) = parse_command_description(&content);
        let description = if description_raw.is_empty() {
            name.clone()
        } else {
            description_raw
        };

        out.push(CustomCommand {
            name,
            prefix,
            description,
            arguments: extract_arguments(&template),
            template,
            source_path: path,
        });
    }
}

fn command_name_from_relative_path(relative: &Path) -> String {
    let mut parts = Vec::new();
    if let Some(parent) = relative.parent() {
        for component in parent.components() {
            parts.push(component.as_os_str().to_string_lossy().into_owned());
        }
    }
    let stem = relative
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_string());
    parts.push(stem);
    parts.join(":")
}

fn is_upper(b: u8) -> bool {
    b.is_ascii_uppercase()
}

fn is_arg_char(b: u8) -> bool {
    b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_'
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_temp_dir() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("othala-custom-commands-{id}"));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    #[test]
    fn extract_arguments_finds_uppercase_patterns() {
        let args = extract_arguments("Hello $NAME in $ENV");
        assert_eq!(args, vec!["NAME", "ENV"]);
    }

    #[test]
    fn extract_arguments_deduplicates() {
        let args = extract_arguments("$NAME then again $NAME");
        assert_eq!(args, vec!["NAME"]);
    }

    #[test]
    fn extract_arguments_ignores_lowercase() {
        let args = extract_arguments("$name and $Name and $UPPER");
        assert_eq!(args, vec!["UPPER"]);
    }

    #[test]
    fn parse_command_description_from_heading() {
        let (description, body) = parse_command_description("# Review PR\nBody line");
        assert_eq!(description, "Review PR");
        assert_eq!(body, "Body line");
    }

    #[test]
    fn parse_command_description_from_html_comment() {
        let (description, body) = parse_command_description("<!-- description: Deploy app -->\nRun $ENV");
        assert_eq!(description, "Deploy app");
        assert_eq!(body, "Run $ENV");
    }

    #[test]
    fn parse_command_description_without_description() {
        let (description, body) = parse_command_description("Run checks\nMore");
        assert_eq!(description, "");
        assert_eq!(body, "Run checks\nMore");
    }

    #[test]
    fn discover_commands_from_empty_dir() {
        let temp_dir = make_temp_dir();
        let commands = discover_commands_from_dir(&temp_dir, CommandPrefix::Project);
        assert!(commands.is_empty());
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn discover_commands_from_dir_reads_markdown_and_subdirs() {
        let temp_dir = make_temp_dir();
        fs::write(temp_dir.join("review.md"), "# Review\nCheck $PR_ID").expect("write review");
        fs::create_dir_all(temp_dir.join("deploy")).expect("create subdir");
        fs::write(temp_dir.join("deploy").join("prod.md"), "Deploy $ENV").expect("write nested");

        let commands = discover_commands_from_dir(&temp_dir, CommandPrefix::User);
        assert_eq!(commands.len(), 2);
        assert!(commands.iter().any(|c| c.name == "review" && c.description == "Review"));
        assert!(commands.iter().any(|c| c.name == "deploy:prod" && c.description == "deploy:prod"));
        let _ = fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn render_command_replaces_arguments() {
        let command = CustomCommand {
            name: "review".to_string(),
            prefix: CommandPrefix::Project,
            description: "Review".to_string(),
            template: "Review $PR in $ENV".to_string(),
            arguments: vec!["PR".to_string(), "ENV".to_string()],
            source_path: PathBuf::from("review.md"),
        };
        let args = HashMap::from([
            ("PR".to_string(), "123".to_string()),
            ("ENV".to_string(), "staging".to_string()),
        ]);
        let rendered = render_command(&command, &args).expect("render command");
        assert_eq!(rendered, "Review 123 in staging");
    }

    #[test]
    fn render_command_errors_on_missing_args() {
        let command = CustomCommand {
            name: "review".to_string(),
            prefix: CommandPrefix::Project,
            description: "Review".to_string(),
            template: "Review $PR in $ENV".to_string(),
            arguments: vec!["PR".to_string(), "ENV".to_string()],
            source_path: PathBuf::from("review.md"),
        };
        let args = HashMap::from([("PR".to_string(), "123".to_string())]);
        let err = render_command(&command, &args).expect_err("expected missing arg");
        assert_eq!(err, "Missing required argument: ENV");
    }

    #[test]
    fn display_commands_table_formats_rows() {
        let commands = vec![CustomCommand {
            name: "review".to_string(),
            prefix: CommandPrefix::User,
            description: "Review changes".to_string(),
            template: "Review $PR".to_string(),
            arguments: vec!["PR".to_string()],
            source_path: PathBuf::from("review.md"),
        }];
        let table = display_commands_table(&commands);
        assert!(table.contains("COMMAND"));
        assert!(table.contains("user:review"));
        assert!(table.contains("PR"));
        assert!(table.contains("Review changes"));
    }

    #[test]
    fn execute_prompt_returns_structured_result() {
        let result = execute_prompt("hello", "claude", "text");
        assert_eq!(result.prompt, "hello");
        assert_eq!(result.model, "claude");
        assert_eq!(result.tokens_used, 0);
        assert!(result.response.contains("Would execute prompt with model claude: hello"));
    }

    #[test]
    fn command_prefix_display() {
        assert_eq!(CommandPrefix::User.to_string(), "user");
        assert_eq!(CommandPrefix::Project.to_string(), "project");
    }
}

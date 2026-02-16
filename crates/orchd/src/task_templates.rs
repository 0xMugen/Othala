use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskTemplate {
    pub name: String,
    pub description: String,
    pub title_template: String,
    pub model: String,
    pub priority: String,
    pub labels: Vec<String>,
    pub depends_on_templates: Vec<String>,
    pub verify_command: Option<String>,
    pub context_files: Vec<String>,
    pub variables: Vec<TemplateVariable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVariable {
    pub name: String,
    pub description: String,
    pub default_value: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TemplateRegistry {
    pub templates: Vec<TaskTemplate>,
}

impl TemplateRegistry {
    pub fn discover(root: &Path) -> Self {
        Self {
            templates: discover_templates(root),
        }
    }
}

pub fn instantiate(template: &TaskTemplate, vars: &HashMap<String, String>) -> Result<TaskTemplate, String> {
    let mut resolved = HashMap::new();
    for variable in &template.variables {
        match vars.get(&variable.name) {
            Some(value) => {
                resolved.insert(variable.name.clone(), value.clone());
            }
            None => match &variable.default_value {
                Some(default) => {
                    resolved.insert(variable.name.clone(), default.clone());
                }
                None => {
                    if variable.required {
                        return Err(format!("Missing required variable: {}", variable.name));
                    }
                }
            },
        }
    }

    let mut output = template.clone();
    output.name = replace_placeholders(&template.name, &resolved)?;
    output.description = replace_placeholders(&template.description, &resolved)?;
    output.title_template = replace_placeholders(&template.title_template, &resolved)?;
    output.model = replace_placeholders(&template.model, &resolved)?;
    output.priority = replace_placeholders(&template.priority, &resolved)?;
    output.labels = template
        .labels
        .iter()
        .map(|value| replace_placeholders(value, &resolved))
        .collect::<Result<Vec<_>, _>>()?;
    output.depends_on_templates = template
        .depends_on_templates
        .iter()
        .map(|value| replace_placeholders(value, &resolved))
        .collect::<Result<Vec<_>, _>>()?;
    output.verify_command = template
        .verify_command
        .as_ref()
        .map(|value| replace_placeholders(value, &resolved))
        .transpose()?;
    output.context_files = template
        .context_files
        .iter()
        .map(|value| replace_placeholders(value, &resolved))
        .collect::<Result<Vec<_>, _>>()?;

    output.variables = template
        .variables
        .iter()
        .map(|variable| {
            Ok(TemplateVariable {
                name: variable.name.clone(),
                description: replace_placeholders(&variable.description, &resolved)?,
                default_value: variable
                    .default_value
                    .as_ref()
                    .map(|value| replace_placeholders(value, &resolved))
                    .transpose()?,
                required: variable.required,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    Ok(output)
}

pub fn validate_template(template: &TaskTemplate) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    if template.name.trim().is_empty() {
        errors.push("template name must not be empty".to_string());
    }
    if template.title_template.trim().is_empty() {
        errors.push("title_template must not be empty".to_string());
    }

    let mut seen = HashSet::new();
    for variable in &template.variables {
        if variable.name.trim().is_empty() {
            errors.push("variable name must not be empty".to_string());
            continue;
        }
        if !seen.insert(variable.name.clone()) {
            errors.push(format!("duplicate variable definition: {}", variable.name));
        }
        if variable.required && variable.default_value.is_none() {
            errors.push(format!(
                "required variable '{}' is missing default_value",
                variable.name
            ));
        }
    }

    let known: HashSet<&str> = template.variables.iter().map(|v| v.name.as_str()).collect();
    for placeholder in collect_placeholders(template) {
        if !known.contains(placeholder.as_str()) {
            errors.push(format!(
                "placeholder '{}' has no variable definition",
                placeholder
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

pub fn discover_templates(root: &Path) -> Vec<TaskTemplate> {
    let mut templates = Vec::new();

    let mut dirs = vec![root.join(".othala/templates")];
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(Path::new(&home).join(".config/othala/templates"));
    }

    for dir in dirs {
        discover_templates_in_dir(&dir, &mut templates);
    }

    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

pub fn parse_template(content: &str) -> Result<TaskTemplate, String> {
    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0usize;

    let mut name = String::new();
    let mut description = String::new();
    let mut title_template = String::new();
    let mut model = String::new();
    let mut priority = String::new();
    let mut labels = Vec::new();
    let mut depends_on_templates = Vec::new();
    let mut verify_command = None;
    let mut context_files = Vec::new();
    let mut variables = Vec::new();

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        if leading_spaces(line) != 0 {
            return Err(format!("Unexpected indentation at line {}", i + 1));
        }

        let (key, value) = split_key_value(trimmed, i + 1)?;
        match key {
            "name" => {
                name = parse_scalar(value);
                i += 1;
            }
            "description" => {
                description = parse_scalar(value);
                i += 1;
            }
            "title_template" => {
                title_template = parse_scalar(value);
                i += 1;
            }
            "model" => {
                model = parse_scalar(value);
                i += 1;
            }
            "priority" => {
                priority = parse_scalar(value);
                i += 1;
            }
            "verify_command" => {
                let parsed = parse_scalar(value);
                verify_command = if parsed.is_empty() { None } else { Some(parsed) };
                i += 1;
            }
            "labels" => {
                let (next, values) = parse_string_list(&lines, i + 1, 2)?;
                labels = values;
                i = next;
            }
            "depends_on_templates" => {
                let (next, values) = parse_string_list(&lines, i + 1, 2)?;
                depends_on_templates = values;
                i = next;
            }
            "context_files" => {
                let (next, values) = parse_string_list(&lines, i + 1, 2)?;
                context_files = values;
                i = next;
            }
            "variables" => {
                let (next, values) = parse_variables(&lines, i + 1, 2)?;
                variables = values;
                i = next;
            }
            other => {
                return Err(format!("Unknown key '{}' at line {}", other, i + 1));
            }
        }
    }

    Ok(TaskTemplate {
        name,
        description,
        title_template,
        model,
        priority,
        labels,
        depends_on_templates,
        verify_command,
        context_files,
        variables,
    })
}

pub fn display_template(template: &TaskTemplate) -> String {
    let mut out = String::new();
    out.push_str(&format!("Template: {}\n", template.name));
    out.push_str(&format!("Description: {}\n", template.description));
    out.push_str(&format!("Title: {}\n", template.title_template));
    out.push_str(&format!("Model: {}\n", template.model));
    out.push_str(&format!("Priority: {}\n", template.priority));
    out.push_str(&format!("Labels: {}\n", join_or_dash(&template.labels)));
    out.push_str(&format!(
        "Depends On: {}\n",
        join_or_dash(&template.depends_on_templates)
    ));
    out.push_str(&format!(
        "Verify Command: {}\n",
        template.verify_command.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "Context Files: {}\n",
        join_or_dash(&template.context_files)
    ));
    out.push_str("Variables:\n");
    if template.variables.is_empty() {
        out.push_str("  - none\n");
    } else {
        for variable in &template.variables {
            out.push_str(&format!(
                "  - {} (required: {}, default: {}) - {}\n",
                variable.name,
                variable.required,
                variable.default_value.as_deref().unwrap_or("-"),
                variable.description
            ));
        }
    }
    out
}

pub fn display_templates_table(templates: &[TaskTemplate]) -> String {
    if templates.is_empty() {
        return "No task templates found.".to_string();
    }

    let name_width = templates
        .iter()
        .map(|t| t.name.len())
        .max()
        .unwrap_or(0)
        .max("NAME".len());
    let model_width = templates
        .iter()
        .map(|t| t.model.len())
        .max()
        .unwrap_or(0)
        .max("MODEL".len());
    let priority_width = templates
        .iter()
        .map(|t| t.priority.len())
        .max()
        .unwrap_or(0)
        .max("PRIORITY".len());

    let mut out = String::new();
    out.push_str(&format!(
        "{:<name_width$}  {:<model_width$}  {:<priority_width$}  TITLE\n",
        "NAME", "MODEL", "PRIORITY"
    ));
    out.push_str(&format!(
        "{}  {}  {}  {}\n",
        "-".repeat(name_width),
        "-".repeat(model_width),
        "-".repeat(priority_width),
        "-".repeat("TITLE".len())
    ));

    for template in templates {
        out.push_str(&format!(
            "{:<name_width$}  {:<model_width$}  {:<priority_width$}  {}\n",
            template.name, template.model, template.priority, template.title_template
        ));
    }

    out
}

fn discover_templates_in_dir(dir: &Path, templates: &mut Vec<TaskTemplate>) {
    if !dir.is_dir() {
        return;
    }

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            discover_templates_in_dir(&path, templates);
            continue;
        }

        let is_yaml = matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("yaml" | "yml")
        );
        if !is_yaml {
            continue;
        }

        let Ok(content) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(template) = parse_template(&content) {
            templates.push(template);
        }
    }
}

fn replace_placeholders(input: &str, vars: &HashMap<String, String>) -> Result<String, String> {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut i = 0usize;

    while i < bytes.len() {
        if i + 1 < bytes.len() && bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            let mut found = false;
            while end + 1 < bytes.len() {
                if bytes[end] == b'}' && bytes[end + 1] == b'}' {
                    found = true;
                    break;
                }
                end += 1;
            }

            if !found {
                return Err("Unclosed template placeholder".to_string());
            }

            let key = input[start..end].trim();
            if key.is_empty() {
                return Err("Empty template placeholder".to_string());
            }

            let value = vars
                .get(key)
                .ok_or_else(|| format!("Missing variable value for '{}'", key))?;
            out.push_str(value);
            i = end + 2;
            continue;
        }

        out.push(bytes[i] as char);
        i += 1;
    }

    Ok(out)
}

fn collect_placeholders(template: &TaskTemplate) -> HashSet<String> {
    let mut placeholders = HashSet::new();

    add_placeholders(&template.name, &mut placeholders);
    add_placeholders(&template.description, &mut placeholders);
    add_placeholders(&template.title_template, &mut placeholders);
    add_placeholders(&template.model, &mut placeholders);
    add_placeholders(&template.priority, &mut placeholders);
    for value in &template.labels {
        add_placeholders(value, &mut placeholders);
    }
    for value in &template.depends_on_templates {
        add_placeholders(value, &mut placeholders);
    }
    if let Some(value) = &template.verify_command {
        add_placeholders(value, &mut placeholders);
    }
    for value in &template.context_files {
        add_placeholders(value, &mut placeholders);
    }

    placeholders
}

fn add_placeholders(input: &str, output: &mut HashSet<String>) {
    let bytes = input.as_bytes();
    let mut i = 0usize;
    while i + 1 < bytes.len() {
        if bytes[i] == b'{' && bytes[i + 1] == b'{' {
            let start = i + 2;
            let mut end = start;
            while end + 1 < bytes.len() {
                if bytes[end] == b'}' && bytes[end + 1] == b'}' {
                    let key = input[start..end].trim();
                    if !key.is_empty() {
                        output.insert(key.to_string());
                    }
                    i = end + 2;
                    break;
                }
                end += 1;
            }
            if end + 1 >= bytes.len() {
                break;
            }
            continue;
        }
        i += 1;
    }
}

fn split_key_value(line: &str, line_no: usize) -> Result<(&str, &str), String> {
    let Some((key, value)) = line.split_once(':') else {
        return Err(format!("Expected key:value at line {}", line_no));
    };
    Ok((key.trim(), value.trim()))
}

fn parse_scalar(raw: &str) -> String {
    let value = raw.trim();
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[value.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[value.len() - 1] == b'\'')
        {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn parse_string_list(lines: &[&str], mut i: usize, min_indent: usize) -> Result<(usize, Vec<String>), String> {
    let mut out = Vec::new();
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        let indent = leading_spaces(line);
        if indent < min_indent {
            break;
        }
        if indent > min_indent {
            return Err(format!("Invalid list indentation at line {}", i + 1));
        }

        let item = trimmed
            .strip_prefix('-')
            .ok_or_else(|| format!("Expected list item at line {}", i + 1))?;
        out.push(parse_scalar(item));
        i += 1;
    }
    Ok((i, out))
}

fn parse_variables(
    lines: &[&str],
    mut i: usize,
    min_indent: usize,
) -> Result<(usize, Vec<TemplateVariable>), String> {
    let mut out = Vec::new();

    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }

        let indent = leading_spaces(line);
        if indent < min_indent {
            break;
        }
        if indent != min_indent {
            return Err(format!("Invalid variables indentation at line {}", i + 1));
        }

        let Some(item) = trimmed.strip_prefix('-') else {
            return Err(format!("Expected variable list entry at line {}", i + 1));
        };
        let initial = item.trim();

        let mut variable = TemplateVariable {
            name: String::new(),
            description: String::new(),
            default_value: None,
            required: false,
        };

        if !initial.is_empty() {
            let (key, value) = split_key_value(initial, i + 1)?;
            assign_variable_field(&mut variable, key, value, i + 1)?;
        }
        i += 1;

        while i < lines.len() {
            let sub_line = lines[i];
            let sub_trimmed = sub_line.trim();
            if sub_trimmed.is_empty() || sub_trimmed.starts_with('#') {
                i += 1;
                continue;
            }

            let sub_indent = leading_spaces(sub_line);
            if sub_indent <= min_indent {
                break;
            }
            if sub_indent != min_indent + 2 {
                return Err(format!("Invalid variable field indentation at line {}", i + 1));
            }

            let (key, value) = split_key_value(sub_trimmed, i + 1)?;
            assign_variable_field(&mut variable, key, value, i + 1)?;
            i += 1;
        }

        out.push(variable);
    }

    Ok((i, out))
}

fn assign_variable_field(
    variable: &mut TemplateVariable,
    key: &str,
    value: &str,
    line_no: usize,
) -> Result<(), String> {
    match key {
        "name" => variable.name = parse_scalar(value),
        "description" => variable.description = parse_scalar(value),
        "default_value" => {
            let parsed = parse_scalar(value);
            variable.default_value = if parsed.is_empty() { None } else { Some(parsed) };
        }
        "required" => {
            variable.required = parse_bool(value)
                .ok_or_else(|| format!("Invalid bool for required at line {}", line_no))?;
        }
        other => {
            return Err(format!("Unknown variable field '{}' at line {}", other, line_no));
        }
    }
    Ok(())
}

fn parse_bool(input: &str) -> Option<bool> {
    match parse_scalar(input).to_ascii_lowercase().as_str() {
        "true" | "yes" | "1" => Some(true),
        "false" | "no" | "0" => Some(false),
        _ => None,
    }
}

fn leading_spaces(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ').count()
}

fn join_or_dash(values: &[String]) -> String {
    if values.is_empty() {
        "-".to_string()
    } else {
        values.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_temp_dir(name: &str) -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("othala-{name}-{id}"));
        if path.exists() {
            let _ = fs::remove_dir_all(&path);
        }
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn sample_template_yaml() -> &'static str {
        r#"name: starter
description: Shared starter template
title_template: "Implement {{FEATURE}}"
model: {{MODEL}}
priority: high
labels:
  - backend
  - {{TEAM}}
depends_on_templates:
  - prep
verify_command: cargo test -p orchd
context_files:
  - crates/orchd/src/lib.rs
variables:
  - name: FEATURE
    description: Target feature
    default_value: task templates
    required: true
  - name: MODEL
    description: model id
    default_value: codex
    required: true
  - name: TEAM
    description: team label
    required: false
"#
    }

    #[test]
    fn parse_template_parses_all_fields() {
        let template = parse_template(sample_template_yaml()).expect("parse template");
        assert_eq!(template.name, "starter");
        assert_eq!(template.title_template, "Implement {{FEATURE}}");
        assert_eq!(template.model, "{{MODEL}}");
        assert_eq!(template.labels, vec!["backend", "{{TEAM}}"]);
        assert_eq!(template.depends_on_templates, vec!["prep"]);
        assert_eq!(template.verify_command.as_deref(), Some("cargo test -p orchd"));
        assert_eq!(template.variables.len(), 3);
    }

    #[test]
    fn parse_template_rejects_unknown_key() {
        let err = parse_template("name: x\nunknown: y\n").expect_err("unknown key should fail");
        assert!(err.contains("Unknown key 'unknown'"));
    }

    #[test]
    fn instantiate_replaces_placeholders() {
        let template = parse_template(sample_template_yaml()).expect("parse template");
        let vars = HashMap::from([
            ("FEATURE".to_string(), "template engine".to_string()),
            ("TEAM".to_string(), "platform".to_string()),
        ]);
        let instantiated = instantiate(&template, &vars).expect("instantiate");
        assert_eq!(instantiated.title_template, "Implement template engine");
        assert_eq!(instantiated.model, "codex");
        assert_eq!(instantiated.labels, vec!["backend", "platform"]);
    }

    #[test]
    fn instantiate_errors_when_required_variable_missing() {
        let template = TaskTemplate {
            name: "missing-default".to_string(),
            description: String::new(),
            title_template: "Run {{REQ}}".to_string(),
            model: "codex".to_string(),
            priority: "high".to_string(),
            labels: Vec::new(),
            depends_on_templates: Vec::new(),
            verify_command: None,
            context_files: Vec::new(),
            variables: vec![TemplateVariable {
                name: "REQ".to_string(),
                description: String::new(),
                default_value: None,
                required: true,
            }],
        };

        let err = instantiate(&template, &HashMap::new()).expect_err("missing required variable");
        assert_eq!(err, "Missing required variable: REQ");
    }

    #[test]
    fn validate_template_accepts_valid_template() {
        let template = parse_template(sample_template_yaml()).expect("parse template");
        assert!(validate_template(&template).is_ok());
    }

    #[test]
    fn validate_template_reports_empty_title() {
        let mut template = parse_template(sample_template_yaml()).expect("parse template");
        template.title_template = "   ".to_string();
        let errors = validate_template(&template).expect_err("title should fail validation");
        assert!(errors.iter().any(|err| err.contains("title_template must not be empty")));
    }

    #[test]
    fn validate_template_reports_required_variable_without_default() {
        let mut template = parse_template(sample_template_yaml()).expect("parse template");
        template.variables[0].default_value = None;
        let errors = validate_template(&template).expect_err("missing default should fail");
        assert!(
            errors
                .iter()
                .any(|err| err.contains("required variable 'FEATURE' is missing default_value"))
        );
    }

    #[test]
    fn validate_template_reports_missing_placeholder_definition() {
        let mut template = parse_template(sample_template_yaml()).expect("parse template");
        template.variables.retain(|v| v.name != "TEAM");
        let errors = validate_template(&template).expect_err("missing variable definition");
        assert!(
            errors
                .iter()
                .any(|err| err.contains("placeholder 'TEAM' has no variable definition"))
        );
    }

    #[test]
    fn discover_templates_loads_project_and_user_templates() {
        let root = make_temp_dir("templates-root");
        let home = make_temp_dir("templates-home");
        let old_home = std::env::var_os("HOME");
        std::env::set_var("HOME", &home);

        let project_dir = root.join(".othala/templates");
        fs::create_dir_all(&project_dir).expect("create project templates dir");
        fs::write(project_dir.join("project.yaml"), sample_template_yaml()).expect("write project template");

        let user_dir = home.join(".config/othala/templates");
        fs::create_dir_all(&user_dir).expect("create user templates dir");
        fs::write(user_dir.join("user.yml"), sample_template_yaml().replace("starter", "user-template"))
            .expect("write user template");

        let templates = discover_templates(&root);
        assert_eq!(templates.len(), 2);
        assert!(templates.iter().any(|t| t.name == "starter"));
        assert!(templates.iter().any(|t| t.name == "user-template"));

        match old_home {
            Some(value) => std::env::set_var("HOME", value),
            None => std::env::remove_var("HOME"),
        }
        let _ = fs::remove_dir_all(root);
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn display_template_contains_key_details() {
        let template = parse_template(sample_template_yaml()).expect("parse template");
        let display = display_template(&template);
        assert!(display.contains("Template: starter"));
        assert!(display.contains("Title: Implement {{FEATURE}}"));
        assert!(display.contains("Variables:"));
    }

    #[test]
    fn display_templates_table_formats_rows() {
        let templates = vec![
            TaskTemplate {
                name: "alpha".to_string(),
                description: String::new(),
                title_template: "Do alpha".to_string(),
                model: "codex".to_string(),
                priority: "high".to_string(),
                labels: Vec::new(),
                depends_on_templates: Vec::new(),
                verify_command: None,
                context_files: Vec::new(),
                variables: Vec::new(),
            },
            TaskTemplate {
                name: "beta".to_string(),
                description: String::new(),
                title_template: "Do beta".to_string(),
                model: "claude".to_string(),
                priority: "medium".to_string(),
                labels: Vec::new(),
                depends_on_templates: Vec::new(),
                verify_command: None,
                context_files: Vec::new(),
                variables: Vec::new(),
            },
        ];

        let table = display_templates_table(&templates);
        assert!(table.contains("NAME"));
        assert!(table.contains("alpha"));
        assert!(table.contains("Do beta"));
    }

    #[test]
    fn template_registry_discovers_templates() {
        let root = make_temp_dir("registry-root");
        let templates_dir = root.join(".othala/templates");
        fs::create_dir_all(&templates_dir).expect("create templates dir");
        fs::write(templates_dir.join("one.yaml"), sample_template_yaml()).expect("write template");

        let registry = TemplateRegistry::discover(&root);
        assert_eq!(registry.templates.len(), 1);
        assert_eq!(registry.templates[0].name, "starter");

        let _ = fs::remove_dir_all(root);
    }
}

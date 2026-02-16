use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditorConfig {
    pub editor_command: Option<String>,
    pub temp_dir: Option<PathBuf>,
    pub file_extension: String,
    pub max_attachment_size_bytes: u64,
    pub allowed_attachment_extensions: Vec<String>,
}

impl Default for EditorConfig {
    fn default() -> Self {
        Self {
            editor_command: None,
            temp_dir: None,
            file_extension: "md".to_string(),
            max_attachment_size_bytes: 10 * 1024 * 1024,
            allowed_attachment_extensions: vec![
                "rs", "py", "js", "ts", "go", "java", "c", "cpp", "h", "hpp", "toml",
                "yaml", "yml", "json", "xml", "html", "css", "md", "txt", "log", "sh",
                "bash", "png", "jpg", "jpeg", "gif", "svg", "pdf",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentInfo {
    pub path: PathBuf,
    pub file_name: String,
    pub extension: String,
    pub size_bytes: u64,
    pub is_binary: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub info: AttachmentInfo,
    pub content: AttachmentContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AttachmentContent {
    Text(String),
    Binary { base64: String, mime_type: String },
}

impl Attachment {
    pub fn display_summary(&self) -> String {
        let content_kind = match self.content {
            AttachmentContent::Text(_) => "text",
            AttachmentContent::Binary { .. } => "binary",
        };
        format!(
            "{} ({}; {}; {})",
            self.info.file_name,
            self.info.extension,
            format_size(self.info.size_bytes),
            content_kind
        )
    }
}

pub fn resolve_editor(config: &EditorConfig) -> Option<String> {
    resolve_editor_with_env(
        config,
        std::env::var("VISUAL").ok(),
        std::env::var("EDITOR").ok(),
    )
}

fn resolve_editor_with_env(
    config: &EditorConfig,
    visual: Option<String>,
    editor: Option<String>,
) -> Option<String> {
    let configured = config
        .editor_command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if configured.is_some() {
        return configured;
    }

    let visual = visual
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if visual.is_some() {
        return visual;
    }

    let editor = editor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    if editor.is_some() {
        return editor;
    }

    Some("vi".to_string())
}

pub fn open_editor(config: &EditorConfig, initial_content: &str) -> Result<String, String> {
    let editor_command = resolve_editor(config).unwrap_or_else(|| "vi".to_string());
    let temp_path = create_temp_editor_file(config, initial_content)?;

    let run_result = run_editor_command(&editor_command, &temp_path);
    let read_result = fs::read_to_string(&temp_path)
        .map_err(|err| format!("Failed to read editor result {}: {err}", temp_path.display()));
    let _ = fs::remove_file(&temp_path);

    run_result?;
    read_result
}

pub fn open_editor_for_prompt(config: &EditorConfig, task_title: &str) -> Result<String, String> {
    let template = build_prompt_template(task_title);
    open_editor(config, &template)
}

pub fn validate_attachment(path: &Path, config: &EditorConfig) -> Result<AttachmentInfo, String> {
    if !path.exists() {
        return Err(format!("Attachment does not exist: {}", path.display()));
    }
    if !path.is_file() {
        return Err(format!("Attachment is not a file: {}", path.display()));
    }

    let metadata = fs::metadata(path)
        .map_err(|err| format!("Failed to read attachment metadata {}: {err}", path.display()))?;
    let size_bytes = metadata.len();
    if size_bytes > config.max_attachment_size_bytes {
        return Err(format!(
            "Attachment too large: {} (max {})",
            format_size(size_bytes),
            format_size(config.max_attachment_size_bytes)
        ));
    }

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Attachment has invalid file name: {}", path.display()))?
        .to_string();

    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase())
        .ok_or_else(|| format!("Attachment missing extension: {}", path.display()))?;

    if !config
        .allowed_attachment_extensions
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(&extension))
    {
        return Err(format!(
            "Attachment extension not allowed: .{} ({})",
            extension,
            path.display()
        ));
    }

    Ok(AttachmentInfo {
        path: path.to_path_buf(),
        file_name,
        extension: extension.clone(),
        size_bytes,
        is_binary: is_binary_extension(&extension),
    })
}

pub fn read_attachment(path: &Path, config: &EditorConfig) -> Result<Attachment, String> {
    let info = validate_attachment(path, config)?;
    let content = if info.is_binary {
        let bytes = fs::read(path)
            .map_err(|err| format!("Failed to read binary attachment {}: {err}", path.display()))?;
        AttachmentContent::Binary {
            base64: encode_base64(&bytes),
            mime_type: mime_type_for_extension(&info.extension).to_string(),
        }
    } else {
        let text = fs::read_to_string(path)
            .map_err(|err| format!("Failed to read text attachment {}: {err}", path.display()))?;
        AttachmentContent::Text(text)
    };

    Ok(Attachment { info, content })
}

pub fn mime_type_for_extension(ext: &str) -> &str {
    match ext.to_ascii_lowercase().as_str() {
        "rs" | "py" | "js" | "ts" | "go" | "java" | "c" | "cpp" | "h" | "hpp" | "sh"
        | "bash" | "txt" | "log" | "md" | "toml" | "yaml" | "yml" | "json" | "xml"
        | "html" | "css" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

pub fn is_binary_extension(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "pdf"
    )
}

pub fn format_size(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }

    let kb = bytes as f64 / 1024.0;
    if kb < 1024.0 {
        return format_unit(kb, "KB");
    }

    let mb = kb / 1024.0;
    if mb < 1024.0 {
        return format_unit(mb, "MB");
    }

    let gb = mb / 1024.0;
    format_unit(gb, "GB")
}

fn format_unit(value: f64, unit: &str) -> String {
    if (value.fract() - 0.0).abs() < f64::EPSILON {
        format!("{value:.0} {unit}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn build_prompt_template(task_title: &str) -> String {
    format!(
        "# {task_title}\n\nWrite your prompt below.\n\n## Context\n-\n\n## Requirements\n-\n"
    )
}

fn create_temp_editor_file(config: &EditorConfig, initial_content: &str) -> Result<PathBuf, String> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);

    let base_dir = config
        .temp_dir
        .clone()
        .unwrap_or_else(std::env::temp_dir);
    fs::create_dir_all(&base_dir)
        .map_err(|err| format!("Failed to create temp directory {}: {err}", base_dir.display()))?;

    let ext = config.file_extension.trim().trim_start_matches('.');
    let ext = if ext.is_empty() { "txt" } else { ext };

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);

    let file_path = base_dir.join(format!("othala-editor-{id}-{nanos}.{ext}"));
    fs::write(&file_path, initial_content).map_err(|err| {
        format!(
            "Failed to write editor temp file {}: {err}",
            file_path.display()
        )
    })?;

    Ok(file_path)
}

fn run_editor_command(editor_command: &str, file_path: &Path) -> Result<(), String> {
    let mut tokens = editor_command.split_whitespace();
    let executable = tokens
        .next()
        .ok_or_else(|| "Editor command is empty".to_string())?;

    let mut args: Vec<OsString> = tokens.map(OsString::from).collect();
    args.push(file_path.as_os_str().to_os_string());

    let status = Command::new(executable)
        .args(&args)
        .status()
        .map_err(|err| format!("Failed to launch editor '{editor_command}': {err}"))?;

    if !status.success() {
        return Err(format!(
            "Editor '{editor_command}' exited with status {status}"
        ));
    }

    Ok(())
}

fn encode_base64(data: &[u8]) -> String {
    if data.is_empty() {
        return String::new();
    }

    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);

    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let n = (b0 << 16) | (b1 << 8) | b2;

        out.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        out.push(TABLE[((n >> 12) & 0x3f) as usize] as char);

        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }

        if chunk.len() > 2 {
            out.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            out.push('=');
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn make_temp_dir() -> PathBuf {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("othala-editor-tests-{id}"));
        if dir.exists() {
            let _ = fs::remove_dir_all(&dir);
        }
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn resolve_editor_uses_config_override() {
        let cfg = EditorConfig {
            editor_command: Some("nano".to_string()),
            ..EditorConfig::default()
        };
        let editor = resolve_editor_with_env(&cfg, Some("vim".to_string()), Some("ed".to_string()));
        assert_eq!(editor, Some("nano".to_string()));
    }

    #[test]
    fn resolve_editor_falls_back_to_env_values() {
        let cfg = EditorConfig::default();
        let editor = resolve_editor_with_env(&cfg, Some("vim".to_string()), Some("ed".to_string()));
        assert_eq!(editor, Some("vim".to_string()));

        let editor = resolve_editor_with_env(&cfg, None, Some("ed".to_string()));
        assert_eq!(editor, Some("ed".to_string()));
    }

    #[test]
    fn editor_config_default_values() {
        let cfg = EditorConfig::default();
        assert_eq!(cfg.file_extension, "md");
        assert_eq!(cfg.max_attachment_size_bytes, 10 * 1024 * 1024);
        assert!(cfg.allowed_attachment_extensions.iter().any(|v| v == "rs"));
        assert!(cfg.allowed_attachment_extensions.iter().any(|v| v == "pdf"));
    }

    #[test]
    fn validate_attachment_rejects_too_large_file() {
        let dir = make_temp_dir();
        let file = dir.join("large.txt");
        fs::write(&file, "01234567890").expect("write test file");

        let cfg = EditorConfig {
            max_attachment_size_bytes: 5,
            ..EditorConfig::default()
        };
        let err = validate_attachment(&file, &cfg).expect_err("expected size validation error");
        assert!(err.contains("Attachment too large"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_attachment_rejects_unknown_extension() {
        let dir = make_temp_dir();
        let file = dir.join("notes.xyz");
        fs::write(&file, "abc").expect("write test file");

        let err = validate_attachment(&file, &EditorConfig::default())
            .expect_err("expected extension validation error");
        assert!(err.contains("not allowed"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn validate_attachment_accepts_valid_file() {
        let dir = make_temp_dir();
        let file = dir.join("code.rs");
        fs::write(&file, "fn main() {}\n").expect("write test file");

        let info = validate_attachment(&file, &EditorConfig::default()).expect("valid attachment");
        assert_eq!(info.file_name, "code.rs");
        assert_eq!(info.extension, "rs");
        assert!(!info.is_binary);

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn mime_type_for_known_extensions() {
        assert_eq!(mime_type_for_extension("png"), "image/png");
        assert_eq!(mime_type_for_extension("pdf"), "application/pdf");
        assert_eq!(mime_type_for_extension("rs"), "text/plain");
    }

    #[test]
    fn is_binary_extension_true_for_images() {
        assert!(is_binary_extension("png"));
        assert!(is_binary_extension("jpeg"));
        assert!(is_binary_extension("pdf"));
    }

    #[test]
    fn is_binary_extension_false_for_text() {
        assert!(!is_binary_extension("rs"));
        assert!(!is_binary_extension("txt"));
    }

    #[test]
    fn format_size_outputs_human_readable_values() {
        assert_eq!(format_size(256), "256 B");
        assert_eq!(format_size(1024), "1 KB");
        assert_eq!(format_size(1536), "1.5 KB");
        assert_eq!(format_size(1024 * 1024), "1 MB");
    }

    #[test]
    fn attachment_content_variants_work() {
        let text_attachment = Attachment {
            info: AttachmentInfo {
                path: PathBuf::from("note.txt"),
                file_name: "note.txt".to_string(),
                extension: "txt".to_string(),
                size_bytes: 4,
                is_binary: false,
            },
            content: AttachmentContent::Text("test".to_string()),
        };
        assert!(text_attachment.display_summary().contains("text"));

        let bin_attachment = Attachment {
            info: AttachmentInfo {
                path: PathBuf::from("image.png"),
                file_name: "image.png".to_string(),
                extension: "png".to_string(),
                size_bytes: 3,
                is_binary: true,
            },
            content: AttachmentContent::Binary {
                base64: "AQID".to_string(),
                mime_type: "image/png".to_string(),
            },
        };
        assert!(bin_attachment.display_summary().contains("binary"));
    }

    #[test]
    fn open_editor_for_prompt_uses_template_generation() {
        let dir = make_temp_dir();
        let cfg = EditorConfig {
            editor_command: Some("true".to_string()),
            temp_dir: Some(dir.clone()),
            ..EditorConfig::default()
        };

        let content = open_editor_for_prompt(&cfg, "Task Alpha").expect("editor prompt content");
        assert!(content.contains("# Task Alpha"));
        assert!(content.contains("Write your prompt below."));
        assert!(content.contains("## Requirements"));

        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn read_attachment_reads_text_and_binary() {
        let dir = make_temp_dir();
        let text_file = dir.join("a.txt");
        let bin_file = dir.join("b.png");
        fs::write(&text_file, "hello").expect("write text");
        fs::write(&bin_file, [1_u8, 2_u8, 3_u8]).expect("write bin");

        let text = read_attachment(&text_file, &EditorConfig::default()).expect("read text attachment");
        match text.content {
            AttachmentContent::Text(v) => assert_eq!(v, "hello"),
            AttachmentContent::Binary { .. } => panic!("expected text"),
        }

        let binary = read_attachment(&bin_file, &EditorConfig::default()).expect("read binary attachment");
        match binary.content {
            AttachmentContent::Binary { base64, mime_type } => {
                assert_eq!(base64, "AQID");
                assert_eq!(mime_type, "image/png");
            }
            AttachmentContent::Text(_) => panic!("expected binary"),
        }

        let _ = fs::remove_dir_all(dir);
    }
}

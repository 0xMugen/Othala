use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::fmt;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[cfg(test)]
use std::collections::VecDeque;
#[cfg(test)]
use std::io::Cursor;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspConfig {
    pub language_servers: HashMap<String, LanguageServerConfig>,
    pub auto_start: bool,
    pub diagnostic_poll_interval_ms: u64,
    pub root_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub root_patterns: Vec<String>,
    pub file_extensions: Vec<String>,
    pub initialization_options: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[repr(u8)]
pub enum DiagnosticSeverity {
    Error = 1,
    Warning = 2,
    Information = 3,
    Hint = 4,
}

impl DiagnosticSeverity {
    fn from_u64(value: u64) -> Option<Self> {
        match value {
            1 => Some(Self::Error),
            2 => Some(Self::Warning),
            3 => Some(Self::Information),
            4 => Some(Self::Hint),
            _ => None,
        }
    }
}

impl fmt::Display for DiagnosticSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Warning => f.write_str("warning"),
            Self::Information => f.write_str("information"),
            Self::Hint => f.write_str("hint"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspDiagnostic {
    pub file_path: String,
    pub line: u32,
    pub column: u32,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub source: Option<String>,
    pub code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LspLocation {
    pub file_path: String,
    pub line: u32,
    pub column: u32,
}

#[derive(Debug)]
pub enum LspError {
    Io(std::io::Error),
    Json(serde_json::Error),
    ServerNotFound(String),
    ServerNotRunning(String),
    ProtocolError(String),
    Timeout,
    InitializationFailed(String),
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "I/O error: {err}"),
            Self::Json(err) => write!(f, "JSON error: {err}"),
            Self::ServerNotFound(language_id) => {
                write!(f, "language server config not found for '{language_id}'")
            }
            Self::ServerNotRunning(language_id) => {
                write!(f, "language server '{language_id}' is not running")
            }
            Self::ProtocolError(message) => write!(f, "LSP protocol error: {message}"),
            Self::Timeout => f.write_str("LSP operation timed out"),
            Self::InitializationFailed(message) => {
                write!(f, "LSP initialization failed: {message}")
            }
        }
    }
}

impl std::error::Error for LspError {}

impl From<std::io::Error> for LspError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for LspError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl Default for LspConfig {
    fn default() -> Self {
        let mut language_servers = HashMap::new();
        language_servers.insert(
            "rust".to_string(),
            LanguageServerConfig {
                command: "rust-analyzer".to_string(),
                args: Vec::new(),
                root_patterns: vec!["Cargo.toml".to_string()],
                file_extensions: vec![".rs".to_string()],
                initialization_options: None,
            },
        );
        language_servers.insert(
            "typescript".to_string(),
            LanguageServerConfig {
                command: "typescript-language-server --stdio".to_string(),
                args: Vec::new(),
                root_patterns: vec!["package.json".to_string(), "tsconfig.json".to_string()],
                file_extensions: vec![".ts".to_string(), ".tsx".to_string()],
                initialization_options: None,
            },
        );
        language_servers.insert(
            "python".to_string(),
            LanguageServerConfig {
                command: "pyright-langserver --stdio".to_string(),
                args: Vec::new(),
                root_patterns: vec!["pyproject.toml".to_string(), "requirements.txt".to_string()],
                file_extensions: vec![".py".to_string()],
                initialization_options: None,
            },
        );

        Self {
            language_servers,
            auto_start: false,
            diagnostic_poll_interval_ms: 2_000,
            root_uri: None,
        }
    }
}

#[derive(Debug, Clone)]
struct OpenDocument {
    language_id: String,
    version: i32,
}

enum LspTransport {
    Process {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<ChildStdout>,
    },
    #[cfg(test)]
    Mock {
        outbound_frames: Vec<Vec<u8>>,
        inbound_frames: VecDeque<Vec<u8>>,
    },
}

pub struct LspClient {
    config: LanguageServerConfig,
    transport: LspTransport,
    next_message_id: u64,
    is_initialized: bool,
    pending_diagnostics: HashMap<String, Vec<LspDiagnostic>>,
}

impl LspClient {
    pub fn start(config: &LanguageServerConfig, root_uri: &str) -> Result<Self, LspError> {
        let (command, embedded_args) = split_command_and_args(&config.command)?;
        let mut all_args = embedded_args;
        all_args.extend(config.args.clone());

        let mut process = Command::new(command);
        process
            .args(all_args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());

        if let Some(root_path) = uri_to_root_path(root_uri) {
            process.current_dir(root_path);
        }

        let mut child = process.spawn()?;
        let stdin = child.stdin.take().ok_or_else(|| {
            LspError::ProtocolError("failed to capture child stdin".to_string())
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            LspError::ProtocolError("failed to capture child stdout".to_string())
        })?;

        Ok(Self {
            config: config.clone(),
            transport: LspTransport::Process {
                child,
                stdin,
                stdout: BufReader::new(stdout),
            },
            next_message_id: 1,
            is_initialized: false,
            pending_diagnostics: HashMap::new(),
        })
    }

    pub fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, LspError> {
        let id = self.next_id();
        let request = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });
        self.write_message(&request)?;

        loop {
            let message = self.read_message()?;
            if let Some(result) = self.handle_incoming_message(message, id)? {
                return Ok(result);
            }
        }
    }

    pub fn send_notification(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<(), LspError> {
        let notification = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        self.write_message(&notification)
    }

    pub fn initialize(
        &mut self,
        root_uri: &str,
        capabilities: serde_json::Value,
    ) -> Result<serde_json::Value, LspError> {
        let result = self.send_request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": capabilities,
                "initializationOptions": self.config.initialization_options,
            }),
        )?;
        self.send_notification("initialized", json!({}))?;
        self.is_initialized = true;
        Ok(result)
    }

    pub fn shutdown(&mut self) -> Result<(), LspError> {
        let _ = self.send_request("shutdown", serde_json::Value::Null)?;
        self.is_initialized = false;
        Ok(())
    }

    pub fn exit(&mut self) -> Result<(), LspError> {
        self.send_notification("exit", serde_json::Value::Null)?;
        match &mut self.transport {
            LspTransport::Process { child, .. } => {
                let _ = child.wait();
            }
            #[cfg(test)]
            LspTransport::Mock { .. } => {}
        }
        Ok(())
    }

    pub fn is_initialized(&self) -> bool {
        self.is_initialized
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_message_id;
        self.next_message_id = self.next_message_id.saturating_add(1);
        id
    }

    fn write_message(&mut self, message: &serde_json::Value) -> Result<(), LspError> {
        let frame = encode_lsp_message(message)?;
        match &mut self.transport {
            LspTransport::Process { stdin, .. } => {
                stdin.write_all(&frame)?;
                stdin.flush()?;
            }
            #[cfg(test)]
            LspTransport::Mock {
                outbound_frames, ..
            } => {
                outbound_frames.push(frame);
            }
        }
        Ok(())
    }

    fn read_message(&mut self) -> Result<serde_json::Value, LspError> {
        match &mut self.transport {
            LspTransport::Process { stdout, .. } => decode_lsp_message(stdout),
            #[cfg(test)]
            LspTransport::Mock { inbound_frames, .. } => {
                let frame = inbound_frames.pop_front().ok_or(LspError::Timeout)?;
                let mut cursor = Cursor::new(frame);
                decode_lsp_message(&mut cursor)
            }
        }
    }

    fn handle_incoming_message(
        &mut self,
        message: serde_json::Value,
        expected_id: u64,
    ) -> Result<Option<serde_json::Value>, LspError> {
        if let Some(method) = message.get("method").and_then(serde_json::Value::as_str) {
            if method == "textDocument/publishDiagnostics" {
                if let Some(params) = message.get("params") {
                    let (file_path, diagnostics) = parse_publish_diagnostics(params)?;
                    self.pending_diagnostics.insert(file_path, diagnostics);
                }
                return Ok(None);
            }
        }

        let Some(raw_id) = message.get("id") else {
            return Ok(None);
        };
        let message_id = parse_message_id(raw_id).ok_or_else(|| {
            LspError::ProtocolError("response id is missing or invalid".to_string())
        })?;
        if message_id != expected_id {
            return Ok(None);
        }

        if let Some(error) = message.get("error") {
            return Err(LspError::ProtocolError(format!(
                "server returned error: {error}"
            )));
        }
        Ok(Some(
            message
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        ))
    }

    fn take_pending_diagnostics(&mut self) -> HashMap<String, Vec<LspDiagnostic>> {
        std::mem::take(&mut self.pending_diagnostics)
    }

    #[cfg(test)]
    fn mock(config: LanguageServerConfig) -> Self {
        Self {
            config,
            transport: LspTransport::Mock {
                outbound_frames: Vec::new(),
                inbound_frames: VecDeque::new(),
            },
            next_message_id: 1,
            is_initialized: false,
            pending_diagnostics: HashMap::new(),
        }
    }

    #[cfg(test)]
    fn mock_push_inbound_message(&mut self, value: serde_json::Value) {
        let frame = encode_lsp_message(&value).expect("encode mock frame");
        if let LspTransport::Mock { inbound_frames, .. } = &mut self.transport {
            inbound_frames.push_back(frame);
        }
    }

    #[cfg(test)]
    fn mock_outbound_messages(&self) -> Vec<serde_json::Value> {
        let frames = match &self.transport {
            LspTransport::Mock {
                outbound_frames, ..
            } => outbound_frames,
            LspTransport::Process { .. } => panic!("process transport does not expose outbound"),
        };

        frames
            .iter()
            .map(|frame| {
                let mut cursor = Cursor::new(frame.clone());
                decode_lsp_message(&mut cursor).expect("decode outbound frame")
            })
            .collect()
    }
}

pub struct LspManager {
    config: LspConfig,
    clients: HashMap<String, LspClient>,
    diagnostics_cache: HashMap<String, Vec<LspDiagnostic>>,
    open_documents: HashMap<String, OpenDocument>,
}

impl LspManager {
    pub fn new(config: LspConfig) -> Self {
        Self {
            config,
            clients: HashMap::new(),
            diagnostics_cache: HashMap::new(),
            open_documents: HashMap::new(),
        }
    }

    pub fn start_server(&mut self, language_id: &str) -> Result<(), LspError> {
        if self.clients.contains_key(language_id) {
            return Ok(());
        }

        let server_config = self
            .config
            .language_servers
            .get(language_id)
            .ok_or_else(|| LspError::ServerNotFound(language_id.to_string()))?
            .clone();

        let root_uri = self.resolve_root_uri();
        let mut client = LspClient::start(&server_config, &root_uri)?;
        let init_result = client.initialize(&root_uri, json!({}));
        if let Err(err) = init_result {
            return Err(LspError::InitializationFailed(err.to_string()));
        }

        self.clients.insert(language_id.to_string(), client);
        Ok(())
    }

    pub fn stop_server(&mut self, language_id: &str) -> Result<(), LspError> {
        let mut client = self
            .clients
            .remove(language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.to_string()))?;
        client.shutdown()?;
        client.exit()?;
        let updates = client.take_pending_diagnostics();
        self.apply_diagnostic_updates(updates);
        Ok(())
    }

    pub fn stop_all(&mut self) -> Vec<(String, Result<(), LspError>)> {
        let mut language_ids = self.clients.keys().cloned().collect::<Vec<_>>();
        language_ids.sort();

        let mut outcomes = Vec::with_capacity(language_ids.len());
        for language_id in language_ids {
            let result = self.stop_server(&language_id);
            outcomes.push((language_id, result));
        }
        outcomes
    }

    pub fn get_diagnostics(&self, file_path: &str) -> Vec<LspDiagnostic> {
        self.diagnostics_cache
            .get(file_path)
            .cloned()
            .unwrap_or_default()
    }

    pub fn get_all_diagnostics(&self) -> HashMap<String, Vec<LspDiagnostic>> {
        self.diagnostics_cache.clone()
    }

    pub fn goto_definition(
        &mut self,
        file_path: &str,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspError> {
        let language_id = self.resolve_language_id(file_path)?;
        let client = self
            .clients
            .get_mut(&language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.clone()))?;

        let result = client.send_request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": file_path_to_uri(file_path) },
                "position": { "line": line, "character": column }
            }),
        )?;
        self.flush_diagnostics_for_language(&language_id);
        Ok(parse_locations(&result))
    }

    pub fn find_references(
        &mut self,
        file_path: &str,
        line: u32,
        column: u32,
    ) -> Result<Vec<LspLocation>, LspError> {
        let language_id = self.resolve_language_id(file_path)?;
        let client = self
            .clients
            .get_mut(&language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.clone()))?;

        let result = client.send_request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": file_path_to_uri(file_path) },
                "position": { "line": line, "character": column },
                "context": { "includeDeclaration": true }
            }),
        )?;
        self.flush_diagnostics_for_language(&language_id);
        Ok(parse_locations(&result))
    }

    pub fn did_open(
        &mut self,
        file_path: &str,
        language_id: &str,
        content: &str,
    ) -> Result<(), LspError> {
        if !self.clients.contains_key(language_id) {
            self.start_server(language_id)?;
        }
        let client = self
            .clients
            .get_mut(language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.to_string()))?;

        client.send_notification(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": file_path_to_uri(file_path),
                    "languageId": language_id,
                    "version": 1,
                    "text": content
                }
            }),
        )?;
        self.open_documents.insert(
            file_path.to_string(),
            OpenDocument {
                language_id: language_id.to_string(),
                version: 1,
            },
        );
        self.flush_diagnostics_for_language(language_id);
        Ok(())
    }

    pub fn did_change(&mut self, file_path: &str, content: &str) -> Result<(), LspError> {
        let document = self
            .open_documents
            .get_mut(file_path)
            .ok_or_else(|| LspError::ProtocolError(format!("file not open: {file_path}")))?;

        document.version += 1;
        let language_id = document.language_id.clone();
        let version = document.version;
        let client = self
            .clients
            .get_mut(&language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.clone()))?;
        client.send_notification(
            "textDocument/didChange",
            json!({
                "textDocument": {
                    "uri": file_path_to_uri(file_path),
                    "version": version
                },
                "contentChanges": [{ "text": content }]
            }),
        )?;
        self.flush_diagnostics_for_language(&language_id);
        Ok(())
    }

    pub fn did_save(&mut self, file_path: &str) -> Result<(), LspError> {
        let document = self
            .open_documents
            .get(file_path)
            .ok_or_else(|| LspError::ProtocolError(format!("file not open: {file_path}")))?;
        let language_id = document.language_id.clone();
        let client = self
            .clients
            .get_mut(&language_id)
            .ok_or_else(|| LspError::ServerNotRunning(language_id.clone()))?;

        client.send_notification(
            "textDocument/didSave",
            json!({
                "textDocument": { "uri": file_path_to_uri(file_path) }
            }),
        )?;
        self.flush_diagnostics_for_language(&language_id);
        Ok(())
    }

    pub fn active_servers(&self) -> Vec<(String, bool)> {
        let mut language_ids = self
            .config
            .language_servers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for running in self.clients.keys() {
            if !language_ids.contains(running) {
                language_ids.push(running.clone());
            }
        }
        language_ids.sort();

        language_ids
            .into_iter()
            .map(|language_id| {
                let initialized = self
                    .clients
                    .get(&language_id)
                    .map(LspClient::is_initialized)
                    .unwrap_or(false);
                (language_id, initialized)
            })
            .collect()
    }

    fn resolve_language_id(&self, file_path: &str) -> Result<String, LspError> {
        if let Some(open) = self.open_documents.get(file_path) {
            return Ok(open.language_id.clone());
        }
        self.language_for_file(file_path)
            .ok_or_else(|| LspError::ServerNotFound(file_path.to_string()))
    }

    fn language_for_file(&self, file_path: &str) -> Option<String> {
        self.config
            .language_servers
            .iter()
            .find_map(|(language_id, server)| {
                if server
                    .file_extensions
                    .iter()
                    .any(|ext| matches_extension(file_path, ext))
                {
                    Some(language_id.clone())
                } else {
                    None
                }
            })
    }

    fn flush_diagnostics_for_language(&mut self, language_id: &str) {
        if let Some(client) = self.clients.get_mut(language_id) {
            let updates = client.take_pending_diagnostics();
            self.apply_diagnostic_updates(updates);
        }
    }

    fn apply_diagnostic_updates(&mut self, updates: HashMap<String, Vec<LspDiagnostic>>) {
        for (path, diagnostics) in updates {
            if diagnostics.is_empty() {
                self.diagnostics_cache.remove(&path);
            } else {
                self.diagnostics_cache.insert(path, diagnostics);
            }
        }
    }
}

fn split_command_and_args(command: &str) -> Result<(String, Vec<String>), LspError> {
    let mut parts = command.split_whitespace();
    let executable = parts
        .next()
        .ok_or_else(|| LspError::ProtocolError("language server command is empty".to_string()))?;
    let args = parts.map(ToString::to_string).collect::<Vec<_>>();
    Ok((executable.to_string(), args))
}

fn matches_extension(file_path: &str, extension: &str) -> bool {
    if extension.is_empty() {
        return false;
    }
    let normalized = if extension.starts_with('.') {
        extension.to_string()
    } else {
        format!(".{extension}")
    };
    file_path.ends_with(&normalized)
}

fn encode_lsp_message(payload: &serde_json::Value) -> Result<Vec<u8>, LspError> {
    let body = serde_json::to_vec(payload)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend(body);
    Ok(frame)
}

fn decode_lsp_message<R: BufRead>(reader: &mut R) -> Result<serde_json::Value, LspError> {
    let mut content_length: Option<usize> = None;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)?;
        if bytes_read == 0 {
            return Err(LspError::ProtocolError(
                "unexpected EOF while reading LSP headers".to_string(),
            ));
        }

        if line == "\r\n" || line == "\n" {
            break;
        }

        if let Some(value) = line.strip_prefix("Content-Length:") {
            let parsed = value.trim().parse::<usize>().map_err(|err| {
                LspError::ProtocolError(format!("invalid Content-Length header: {err}"))
            })?;
            content_length = Some(parsed);
        } else if line.to_ascii_lowercase().starts_with("content-length:") {
            let parsed = line
                .split(':')
                .nth(1)
                .map(str::trim)
                .ok_or_else(|| {
                    LspError::ProtocolError("malformed Content-Length header".to_string())
                })?
                .parse::<usize>()
                .map_err(|err| {
                    LspError::ProtocolError(format!("invalid Content-Length header: {err}"))
                })?;
            content_length = Some(parsed);
        }
    }

    let length = content_length
        .ok_or_else(|| LspError::ProtocolError("missing Content-Length header".to_string()))?;
    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    Ok(serde_json::from_slice(&body)?)
}

fn parse_message_id(value: &serde_json::Value) -> Option<u64> {
    if let Some(id) = value.as_u64() {
        return Some(id);
    }
    if let Some(id) = value.as_i64() {
        return u64::try_from(id).ok();
    }
    value.as_str()?.parse::<u64>().ok()
}

fn parse_publish_diagnostics(
    params: &serde_json::Value,
) -> Result<(String, Vec<LspDiagnostic>), LspError> {
    let uri = params
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| LspError::ProtocolError("publishDiagnostics missing uri".to_string()))?;
    let file_path = uri_to_file_path(uri);
    let diagnostics = params
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut parsed = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        let line = diagnostic
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("line"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let column = diagnostic
            .get("range")
            .and_then(|range| range.get("start"))
            .and_then(|start| start.get("character"))
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .unwrap_or(0);
        let severity = diagnostic
            .get("severity")
            .and_then(serde_json::Value::as_u64)
            .and_then(DiagnosticSeverity::from_u64)
            .unwrap_or(DiagnosticSeverity::Warning);
        let message = diagnostic
            .get("message")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let source = diagnostic
            .get("source")
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string);
        let code = diagnostic.get("code").and_then(|value| {
            value
                .as_str()
                .map(ToString::to_string)
                .or_else(|| value.as_u64().map(|v| v.to_string()))
                .or_else(|| value.as_i64().map(|v| v.to_string()))
        });

        parsed.push(LspDiagnostic {
            file_path: file_path.clone(),
            line,
            column,
            severity,
            message,
            source,
            code,
        });
    }
    Ok((file_path, parsed))
}

fn parse_locations(value: &serde_json::Value) -> Vec<LspLocation> {
    if value.is_null() {
        return Vec::new();
    }

    if let Some(items) = value.as_array() {
        return items.iter().filter_map(parse_location_entry).collect();
    }

    parse_location_entry(value).into_iter().collect()
}

fn parse_location_entry(value: &serde_json::Value) -> Option<LspLocation> {
    let uri = value
        .get("uri")
        .and_then(serde_json::Value::as_str)
        .or_else(|| value.get("targetUri").and_then(serde_json::Value::as_str))?;
    let range = value
        .get("range")
        .or_else(|| value.get("targetSelectionRange"))
        .or_else(|| value.get("targetRange"))?;

    let line = range
        .get("start")
        .and_then(|start| start.get("line"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);
    let column = range
        .get("start")
        .and_then(|start| start.get("character"))
        .and_then(serde_json::Value::as_u64)
        .and_then(|v| u32::try_from(v).ok())
        .unwrap_or(0);

    Some(LspLocation {
        file_path: uri_to_file_path(uri),
        line,
        column,
    })
}

fn file_path_to_uri(file_path: &str) -> String {
    if file_path.starts_with("file://") {
        return file_path.to_string();
    }

    let path = Path::new(file_path);
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| Path::new("/").to_path_buf())
            .join(path)
    };
    format!("file://{}", absolute.to_string_lossy().replace('\\', "/"))
}

fn uri_to_file_path(uri: &str) -> String {
    uri.strip_prefix("file://")
        .map(ToString::to_string)
        .unwrap_or_else(|| uri.to_string())
}

fn uri_to_root_path(uri: &str) -> Option<String> {
    uri.strip_prefix("file://").map(ToString::to_string)
}

impl LspManager {
    fn resolve_root_uri(&self) -> String {
        if let Some(uri) = &self.config.root_uri {
            return uri.clone();
        }
        file_path_to_uri(
            &std::env::current_dir()
                .unwrap_or_else(|_| Path::new("/").to_path_buf())
                .to_string_lossy(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_language_server_config() -> LanguageServerConfig {
        LanguageServerConfig {
            command: "rust-analyzer".to_string(),
            args: vec!["--log-file".to_string(), "ra.log".to_string()],
            root_patterns: vec!["Cargo.toml".to_string()],
            file_extensions: vec![".rs".to_string()],
            initialization_options: Some(json!({ "checkOnSave": true })),
        }
    }

    fn manager_with_mock_rust_client() -> LspManager {
        let mut config = LspConfig::default();
        config.language_servers.insert(
            "rust".to_string(),
            LanguageServerConfig {
                command: "rust-analyzer".to_string(),
                args: Vec::new(),
                root_patterns: vec!["Cargo.toml".to_string()],
                file_extensions: vec![".rs".to_string()],
                initialization_options: None,
            },
        );
        let mut manager = LspManager::new(config);
        manager
            .clients
            .insert("rust".to_string(), LspClient::mock(sample_language_server_config()));
        manager
    }

    #[test]
    fn lsp_config_default_contains_common_servers() {
        let config = LspConfig::default();
        assert!(config.language_servers.contains_key("rust"));
        assert!(config.language_servers.contains_key("typescript"));
        assert!(config.language_servers.contains_key("python"));
        assert_eq!(config.diagnostic_poll_interval_ms, 2_000);
    }

    #[test]
    fn lsp_config_serialization_roundtrip() {
        let config = LspConfig::default();
        let value = serde_json::to_value(&config).expect("serialize config");
        let decoded: LspConfig = serde_json::from_value(value).expect("deserialize config");
        assert_eq!(decoded.auto_start, config.auto_start);
        assert_eq!(
            decoded.diagnostic_poll_interval_ms,
            config.diagnostic_poll_interval_ms
        );
        assert!(decoded.language_servers.contains_key("rust"));
    }

    #[test]
    fn lsp_diagnostic_creation_and_severity_ordering() {
        let error = LspDiagnostic {
            file_path: "src/main.rs".to_string(),
            line: 10,
            column: 4,
            severity: DiagnosticSeverity::Error,
            message: "broken".to_string(),
            source: Some("rust-analyzer".to_string()),
            code: Some("E0001".to_string()),
        };
        let warning = LspDiagnostic {
            file_path: "src/main.rs".to_string(),
            line: 12,
            column: 2,
            severity: DiagnosticSeverity::Warning,
            message: "warn".to_string(),
            source: None,
            code: None,
        };

        assert_eq!(error.file_path, "src/main.rs");
        assert!(DiagnosticSeverity::Error < DiagnosticSeverity::Warning);
        assert_eq!(warning.severity as u8, 2);
    }

    #[test]
    fn diagnostic_severity_display_impl() {
        assert_eq!(DiagnosticSeverity::Error.to_string(), "error");
        assert_eq!(DiagnosticSeverity::Warning.to_string(), "warning");
        assert_eq!(DiagnosticSeverity::Information.to_string(), "information");
        assert_eq!(DiagnosticSeverity::Hint.to_string(), "hint");
    }

    #[test]
    fn lsp_manager_creation_and_server_listing() {
        let manager = LspManager::new(LspConfig::default());
        let active = manager.active_servers();
        assert!(active.iter().any(|(language, running)| language == "rust" && !running));
        assert!(active
            .iter()
            .any(|(language, running)| language == "typescript" && !running));
    }

    #[test]
    fn json_rpc_message_framing_roundtrip() {
        let payload = json!({ "jsonrpc": "2.0", "id": 7, "result": {"ok": true} });
        let encoded = encode_lsp_message(&payload).expect("encode frame");
        let mut cursor = Cursor::new(encoded);
        let decoded = decode_lsp_message(&mut cursor).expect("decode frame");
        assert_eq!(decoded, payload);
    }

    #[test]
    fn json_rpc_message_framing_requires_content_length() {
        let broken = b"\r\n{\"jsonrpc\":\"2.0\"}".to_vec();
        let mut cursor = Cursor::new(broken);
        let err = decode_lsp_message(&mut cursor).expect_err("must fail");
        assert!(err.to_string().contains("missing Content-Length"));
    }

    #[test]
    fn request_response_id_matching_ignores_non_matching_id() {
        let mut client = LspClient::mock(sample_language_server_config());
        client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 99, "result": "wrong" }));
        client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": "ok" }));
        let result = client
            .send_request("workspace/executeCommand", json!({ "command": "x" }))
            .expect("request succeeds");
        assert_eq!(result, json!("ok"));
    }

    #[test]
    fn lsp_error_display_impls() {
        let not_found = LspError::ServerNotFound("rust".to_string()).to_string();
        let not_running = LspError::ServerNotRunning("typescript".to_string()).to_string();
        let timeout = LspError::Timeout.to_string();
        assert!(not_found.contains("rust"));
        assert!(not_running.contains("typescript"));
        assert!(timeout.contains("timed out"));
    }

    #[test]
    fn lsp_location_creation() {
        let location = LspLocation {
            file_path: "/repo/src/lib.rs".to_string(),
            line: 42,
            column: 8,
        };
        assert_eq!(location.file_path, "/repo/src/lib.rs");
        assert_eq!(location.line, 42);
        assert_eq!(location.column, 8);
    }

    #[test]
    fn language_server_config_with_root_patterns() {
        let config = LanguageServerConfig {
            command: "typescript-language-server --stdio".to_string(),
            args: vec!["--tsserver-path".to_string(), "node_modules/typescript/lib".to_string()],
            root_patterns: vec!["package.json".to_string(), "tsconfig.json".to_string()],
            file_extensions: vec![".ts".to_string(), ".tsx".to_string()],
            initialization_options: None,
        };
        assert_eq!(config.root_patterns.len(), 2);
        assert!(config.root_patterns.contains(&"package.json".to_string()));
    }

    #[test]
    fn file_extension_matching_to_language_id() {
        let manager = LspManager::new(LspConfig::default());
        assert_eq!(manager.language_for_file("src/main.rs"), Some("rust".to_string()));
        assert_eq!(
            manager.language_for_file("web/app.tsx"),
            Some("typescript".to_string())
        );
        assert_eq!(manager.language_for_file("script.py"), Some("python".to_string()));
        assert_eq!(manager.language_for_file("README.md"), None);
    }

    #[test]
    fn diagnostics_cache_operations_insert_get_clear() {
        let mut manager = LspManager::new(LspConfig::default());
        let path = "src/main.rs".to_string();
        manager.diagnostics_cache.insert(
            path.clone(),
            vec![LspDiagnostic {
                file_path: path.clone(),
                line: 1,
                column: 2,
                severity: DiagnosticSeverity::Error,
                message: "boom".to_string(),
                source: Some("rust-analyzer".to_string()),
                code: Some("E42".to_string()),
            }],
        );

        assert_eq!(manager.get_diagnostics(&path).len(), 1);
        assert_eq!(manager.get_all_diagnostics().len(), 1);
        manager.diagnostics_cache.remove(&path);
        assert!(manager.get_diagnostics(&path).is_empty());
    }

    #[test]
    fn multiple_server_management_stop_all_uses_shutdown_exit() {
        let mut manager = LspManager::new(LspConfig::default());
        let mut rust_client = LspClient::mock(sample_language_server_config());
        rust_client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": null }));
        let mut py_client = LspClient::mock(sample_language_server_config());
        py_client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": null }));

        manager.clients.insert("rust".to_string(), rust_client);
        manager.clients.insert("python".to_string(), py_client);

        let outcomes = manager.stop_all();
        assert_eq!(outcomes.len(), 2);
        assert!(outcomes.into_iter().all(|(_, result)| result.is_ok()));
        assert!(manager.clients.is_empty());
    }

    #[test]
    fn did_open_message_formatting() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn main() {}")
            .expect("did_open succeeds");

        let client = manager.clients.get("rust").expect("rust client exists");
        let outbound = client.mock_outbound_messages();
        assert_eq!(outbound.len(), 1);
        assert_eq!(outbound[0]["method"], json!("textDocument/didOpen"));
        assert_eq!(outbound[0]["params"]["textDocument"]["languageId"], json!("rust"));
        assert_eq!(outbound[0]["params"]["textDocument"]["version"], json!(1));
    }

    #[test]
    fn did_change_message_formatting() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn one() {}")
            .expect("open");
        manager
            .did_change("src/lib.rs", "fn two() {}")
            .expect("change");

        let client = manager.clients.get("rust").expect("rust client");
        let outbound = client.mock_outbound_messages();
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[1]["method"], json!("textDocument/didChange"));
        assert_eq!(
            outbound[1]["params"]["textDocument"]["version"],
            json!(2)
        );
        assert_eq!(
            outbound[1]["params"]["contentChanges"][0]["text"],
            json!("fn two() {}")
        );
    }

    #[test]
    fn did_save_message_formatting() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn one() {}")
            .expect("open");
        manager.did_save("src/lib.rs").expect("save");

        let client = manager.clients.get("rust").expect("rust client");
        let outbound = client.mock_outbound_messages();
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[1]["method"], json!("textDocument/didSave"));
    }

    #[test]
    fn initialize_request_building() {
        let mut client = LspClient::mock(sample_language_server_config());
        client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": {"capabilities":{}} }));

        let result = client
            .initialize("file:///repo", json!({"workspace": {"didChangeWatchedFiles": true}}))
            .expect("initialize succeeds");
        assert_eq!(result["capabilities"], json!({}));

        let outbound = client.mock_outbound_messages();
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[0]["method"], json!("initialize"));
        assert_eq!(outbound[0]["params"]["rootUri"], json!("file:///repo"));
        assert_eq!(outbound[1]["method"], json!("initialized"));
        assert!(client.is_initialized());
    }

    #[test]
    fn shutdown_sequence() {
        let mut client = LspClient::mock(sample_language_server_config());
        client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": null }));
        client.is_initialized = true;

        client.shutdown().expect("shutdown");
        client.exit().expect("exit");
        let outbound = client.mock_outbound_messages();
        assert_eq!(outbound.len(), 2);
        assert_eq!(outbound[0]["method"], json!("shutdown"));
        assert_eq!(outbound[1]["method"], json!("exit"));
        assert!(!client.is_initialized());
    }

    #[test]
    fn publish_diagnostics_notification_updates_cache() {
        let mut manager = manager_with_mock_rust_client();
        let client = manager.clients.get_mut("rust").expect("rust client");
        client.mock_push_inbound_message(json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": {
                "uri": "file:///repo/src/lib.rs",
                "diagnostics": [
                    {
                        "range": { "start": { "line": 3, "character": 5 } },
                        "severity": 1,
                        "message": "bad",
                        "source": "rust-analyzer",
                        "code": "E1"
                    }
                ]
            }
        }));
        client.mock_push_inbound_message(json!({ "jsonrpc": "2.0", "id": 1, "result": null }));

        let _ = client
            .send_request("workspace/executeCommand", json!({"command": "noop"}))
            .expect("request response");
        manager.flush_diagnostics_for_language("rust");

        let diagnostics = manager.get_diagnostics("/repo/src/lib.rs");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, DiagnosticSeverity::Error);
        assert_eq!(diagnostics[0].line, 3);
    }

    #[test]
    fn goto_definition_parses_location_array() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn x() {}")
            .expect("open");
        let client = manager.clients.get_mut("rust").expect("client");
        client.mock_push_inbound_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": [
                {
                    "uri": "file:///repo/src/defs.rs",
                    "range": { "start": { "line": 10, "character": 2 } }
                }
            ]
        }));

        let locations = manager
            .goto_definition("src/lib.rs", 0, 1)
            .expect("goto_definition");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].file_path, "/repo/src/defs.rs");
        assert_eq!(locations[0].line, 10);
    }

    #[test]
    fn goto_definition_parses_location_link() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn x() {}")
            .expect("open");
        let client = manager.clients.get_mut("rust").expect("client");
        client.mock_push_inbound_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": [
                {
                    "targetUri": "file:///repo/src/defs.rs",
                    "targetSelectionRange": { "start": { "line": 4, "character": 7 } }
                }
            ]
        }));

        let locations = manager
            .goto_definition("src/lib.rs", 0, 1)
            .expect("goto_definition");
        assert_eq!(locations.len(), 1);
        assert_eq!(locations[0].column, 7);
    }

    #[test]
    fn find_references_formats_request_and_parses_result() {
        let mut manager = manager_with_mock_rust_client();
        manager
            .did_open("src/lib.rs", "rust", "fn x() {}")
            .expect("open");
        let client = manager.clients.get_mut("rust").expect("client");
        client.mock_push_inbound_message(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": [
                {
                    "uri": "file:///repo/src/lib.rs",
                    "range": { "start": { "line": 1, "character": 0 } }
                },
                {
                    "uri": "file:///repo/src/other.rs",
                    "range": { "start": { "line": 2, "character": 3 } }
                }
            ]
        }));

        let refs = manager
            .find_references("src/lib.rs", 0, 0)
            .expect("find_references");
        assert_eq!(refs.len(), 2);

        let outbound = manager
            .clients
            .get("rust")
            .expect("rust client")
            .mock_outbound_messages();
        assert!(outbound.iter().any(|msg| {
            msg["method"] == json!("textDocument/references")
                && msg["params"]["context"]["includeDeclaration"] == json!(true)
        }));
    }

    #[test]
    fn parse_message_id_accepts_integer_and_string() {
        assert_eq!(parse_message_id(&json!(3)), Some(3));
        assert_eq!(parse_message_id(&json!("12")), Some(12));
        assert_eq!(parse_message_id(&json!(-1)), None);
    }

    #[test]
    fn split_command_and_args_supports_embedded_flags() {
        let (exe, args) = split_command_and_args("typescript-language-server --stdio --log-level 2")
            .expect("split command");
        assert_eq!(exe, "typescript-language-server");
        assert_eq!(args, vec!["--stdio", "--log-level", "2"]);
    }

    #[test]
    fn split_command_and_args_rejects_empty_command() {
        let err = split_command_and_args("  ").expect_err("empty command should fail");
        assert!(err.to_string().contains("command is empty"));
    }

    #[test]
    fn file_path_uri_conversion_roundtrip() {
        let uri = file_path_to_uri("src/main.rs");
        assert!(uri.starts_with("file://"));
        let path = uri_to_file_path(&uri);
        assert!(path.ends_with("src/main.rs"));
    }

    #[test]
    fn active_servers_reports_initialization_state() {
        let mut manager = manager_with_mock_rust_client();
        if let Some(client) = manager.clients.get_mut("rust") {
            client.is_initialized = true;
        }

        let active = manager.active_servers();
        let rust_state = active
            .into_iter()
            .find(|(language, _)| language == "rust")
            .expect("rust listed");
        assert!(rust_state.1);
    }
}

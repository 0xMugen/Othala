use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceDefinition {
    pub uri: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceContent {
    pub uri: String,
    pub mime_type: Option<String>,
    pub text: Option<String>,
    pub blob: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceTemplate {
    pub uri_template: String,
    pub name: String,
    pub description: Option<String>,
    pub mime_type: Option<String>,
}

type ResourceHandler = dyn Fn(&str) -> Result<ResourceContent, ResourceError>;

pub struct ResourceRegistry {
    resources: Vec<ResourceDefinition>,
    templates: Vec<ResourceTemplate>,
    handlers: HashMap<String, Box<ResourceHandler>>,
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self {
            resources: Vec::new(),
            templates: Vec::new(),
            handlers: HashMap::new(),
        }
    }

    pub fn register_resource(
        &mut self,
        definition: ResourceDefinition,
        handler: Box<ResourceHandler>,
    ) {
        self.resources.retain(|existing| existing.uri != definition.uri);
        self.handlers.insert(definition.uri.clone(), handler);
        self.resources.push(definition);
    }

    pub fn register_template(
        &mut self,
        template: ResourceTemplate,
        handler: Box<ResourceHandler>,
    ) {
        self.templates
            .retain(|existing| existing.uri_template != template.uri_template);
        self.handlers.insert(template.uri_template.clone(), handler);
        self.templates.push(template);
    }

    pub fn list_resources(&self) -> &[ResourceDefinition] {
        &self.resources
    }

    pub fn list_templates(&self) -> &[ResourceTemplate] {
        &self.templates
    }

    pub fn read_resource(&self, uri: &str) -> Result<ResourceContent, ResourceError> {
        if !uri.starts_with("othala://") {
            return Err(ResourceError::InvalidUri(uri.to_string()));
        }

        if let Some(handler) = self.handlers.get(uri) {
            return handler(uri).map_err(|err| match err {
                ResourceError::ReadFailed(_) => err,
                other => ResourceError::ReadFailed(other.to_string()),
            });
        }

        for template in &self.templates {
            if uri_matches_template(&template.uri_template, uri) {
                if let Some(handler) = self.handlers.get(&template.uri_template) {
                    return handler(uri).map_err(|err| match err {
                        ResourceError::ReadFailed(_) => err,
                        other => ResourceError::ReadFailed(other.to_string()),
                    });
                }
            }
        }

        Err(ResourceError::NotFound(uri.to_string()))
    }

    pub fn register_builtin_resources(&mut self) {
        self.register_resource(
            ResourceDefinition {
                uri: "othala://tasks".to_string(),
                name: "tasks".to_string(),
                description: Some("Current tasks and state summary".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"tasks\":[]}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://config".to_string(),
                name: "config".to_string(),
                description: Some("Active orchestrator configuration".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"config\":{}}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://events".to_string(),
                name: "events".to_string(),
                description: Some("Recent event log entries".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"events\":[]}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://sessions".to_string(),
                name: "sessions".to_string(),
                description: Some("Known coding sessions".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"sessions\":[]}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://health".to_string(),
                name: "health".to_string(),
                description: Some("Daemon health snapshot".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"status\":\"healthy\"}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://skills".to_string(),
                name: "skills".to_string(),
                description: Some("Installed and builtin skills".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"skills\":[]}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_resource(
            ResourceDefinition {
                uri: "othala://stats".to_string(),
                name: "stats".to_string(),
                description: Some("Runtime metrics and totals".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{\"stats\":{}}".to_string()),
                    blob: None,
                })
            }),
        );

        self.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "task-detail".to_string(),
                description: Some("Task detail by ID".to_string()),
                mime_type: Some("application/json".to_string()),
            },
            Box::new(|uri| {
                Ok(ResourceContent {
                    uri: uri.to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some(format!("{{\"task_uri\":\"{}\"}}", uri)),
                    blob: None,
                })
            }),
        );
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptDefinition {
    pub name: String,
    pub description: Option<String>,
    pub arguments: Vec<PromptArgument>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptArgument {
    pub name: String,
    pub description: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PromptMessage {
    pub role: PromptRole,
    pub content: PromptContent,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PromptRole {
    User,
    Assistant,
}

impl fmt::Display for PromptRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::User => write!(f, "user"),
            Self::Assistant => write!(f, "assistant"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PromptContent {
    Text { text: String },
    Resource { resource: ResourceContent },
}

type PromptHandler = dyn Fn(&HashMap<String, String>) -> Result<Vec<PromptMessage>, ResourceError>;

pub struct PromptRegistry {
    prompts: Vec<PromptDefinition>,
    handlers: HashMap<String, Box<PromptHandler>>,
}

impl Default for PromptRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PromptRegistry {
    pub fn new() -> Self {
        Self {
            prompts: Vec::new(),
            handlers: HashMap::new(),
        }
    }

    pub fn register_prompt(
        &mut self,
        definition: PromptDefinition,
        handler: Box<PromptHandler>,
    ) {
        self.prompts.retain(|existing| existing.name != definition.name);
        self.handlers.insert(definition.name.clone(), handler);
        self.prompts.push(definition);
    }

    pub fn list_prompts(&self) -> &[PromptDefinition] {
        &self.prompts
    }

    pub fn get_prompt(
        &self,
        name: &str,
        args: HashMap<String, String>,
    ) -> Result<Vec<PromptMessage>, ResourceError> {
        let definition = self
            .prompts
            .iter()
            .find(|prompt| prompt.name == name)
            .ok_or_else(|| ResourceError::PromptNotFound(name.to_string()))?;

        for argument in &definition.arguments {
            if argument.required && !args.contains_key(&argument.name) {
                return Err(ResourceError::InvalidArguments(format!(
                    "missing required argument '{}'",
                    argument.name
                )));
            }
        }

        let handler = self
            .handlers
            .get(name)
            .ok_or_else(|| ResourceError::PromptNotFound(name.to_string()))?;
        handler(&args)
    }

    pub fn register_builtin_prompts(&mut self) {
        self.register_prompt(
            PromptDefinition {
                name: "code-review".to_string(),
                description: Some("Review a code change and suggest improvements".to_string()),
                arguments: vec![PromptArgument {
                    name: "diff".to_string(),
                    description: Some("Patch or diff text to review".to_string()),
                    required: true,
                }],
            },
            Box::new(|args| {
                let diff = args
                    .get("diff")
                    .cloned()
                    .ok_or_else(|| ResourceError::InvalidArguments("missing diff".to_string()))?;
                Ok(vec![
                    PromptMessage {
                        role: PromptRole::User,
                        content: PromptContent::Text {
                            text: format!("Please review this diff:\n{}", diff),
                        },
                    },
                    PromptMessage {
                        role: PromptRole::Assistant,
                        content: PromptContent::Text {
                            text: "I will review for correctness, safety, and maintainability."
                                .to_string(),
                        },
                    },
                ])
            }),
        );

        self.register_prompt(
            PromptDefinition {
                name: "bug-fix".to_string(),
                description: Some("Generate a fix strategy for a bug report".to_string()),
                arguments: vec![PromptArgument {
                    name: "issue".to_string(),
                    description: Some("Problem statement".to_string()),
                    required: true,
                }],
            },
            Box::new(|args| {
                let issue = args
                    .get("issue")
                    .cloned()
                    .ok_or_else(|| ResourceError::InvalidArguments("missing issue".to_string()))?;
                Ok(vec![PromptMessage {
                    role: PromptRole::User,
                    content: PromptContent::Text {
                        text: format!("Create a minimal bug fix plan for: {}", issue),
                    },
                }])
            }),
        );

        self.register_prompt(
            PromptDefinition {
                name: "test-generation".to_string(),
                description: Some("Produce test cases for a target module".to_string()),
                arguments: vec![PromptArgument {
                    name: "target".to_string(),
                    description: Some("Module or function target".to_string()),
                    required: true,
                }],
            },
            Box::new(|args| {
                let target = args
                    .get("target")
                    .cloned()
                    .ok_or_else(|| ResourceError::InvalidArguments("missing target".to_string()))?;
                Ok(vec![PromptMessage {
                    role: PromptRole::User,
                    content: PromptContent::Text {
                        text: format!("Generate focused tests for '{}'.", target),
                    },
                }])
            }),
        );

        self.register_prompt(
            PromptDefinition {
                name: "refactor".to_string(),
                description: Some("Suggest a low-risk refactor plan".to_string()),
                arguments: vec![PromptArgument {
                    name: "scope".to_string(),
                    description: Some("Refactor scope".to_string()),
                    required: true,
                }],
            },
            Box::new(|args| {
                let scope = args
                    .get("scope")
                    .cloned()
                    .ok_or_else(|| ResourceError::InvalidArguments("missing scope".to_string()))?;
                Ok(vec![PromptMessage {
                    role: PromptRole::User,
                    content: PromptContent::Text {
                        text: format!("Refactor this scope without changing behavior: {}", scope),
                    },
                }])
            }),
        );

        self.register_prompt(
            PromptDefinition {
                name: "explain".to_string(),
                description: Some("Explain code or architecture clearly".to_string()),
                arguments: vec![PromptArgument {
                    name: "topic".to_string(),
                    description: Some("Thing to explain".to_string()),
                    required: true,
                }],
            },
            Box::new(|args| {
                let topic = args
                    .get("topic")
                    .cloned()
                    .ok_or_else(|| ResourceError::InvalidArguments("missing topic".to_string()))?;
                Ok(vec![PromptMessage {
                    role: PromptRole::User,
                    content: PromptContent::Text {
                        text: format!("Explain this in practical terms: {}", topic),
                    },
                }])
            }),
        );
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ResourceError {
    #[error("resource not found: {0}")]
    NotFound(String),
    #[error("invalid resource uri: {0}")]
    InvalidUri(String),
    #[error("failed to read resource: {0}")]
    ReadFailed(String),
    #[error("invalid prompt arguments: {0}")]
    InvalidArguments(String),
    #[error("prompt not found: {0}")]
    PromptNotFound(String),
}

fn uri_matches_template(template: &str, candidate: &str) -> bool {
    let template_parts: Vec<&str> = template.split('/').collect();
    let candidate_parts: Vec<&str> = candidate.split('/').collect();

    if template_parts.len() != candidate_parts.len() {
        return false;
    }

    template_parts
        .iter()
        .zip(candidate_parts.iter())
        .all(|(left, right)| {
            (left.starts_with('{') && left.ends_with('}')) || left == right
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_content(uri: &str, value: &str) -> ResourceContent {
        ResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/plain".to_string()),
            text: Some(value.to_string()),
            blob: None,
        }
    }

    #[test]
    fn register_resource_adds_resource_definition() {
        let mut registry = ResourceRegistry::new();
        registry.register_resource(
            ResourceDefinition {
                uri: "othala://one".to_string(),
                name: "one".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "one"))),
        );

        assert_eq!(registry.list_resources().len(), 1);
        assert_eq!(registry.list_resources()[0].uri, "othala://one");
    }

    #[test]
    fn register_resource_replaces_existing_uri() {
        let mut registry = ResourceRegistry::new();
        registry.register_resource(
            ResourceDefinition {
                uri: "othala://dup".to_string(),
                name: "first".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "first"))),
        );
        registry.register_resource(
            ResourceDefinition {
                uri: "othala://dup".to_string(),
                name: "second".to_string(),
                description: Some("updated".to_string()),
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "second"))),
        );

        assert_eq!(registry.list_resources().len(), 1);
        assert_eq!(registry.list_resources()[0].name, "second");
        let read = registry
            .read_resource("othala://dup")
            .expect("resource should resolve");
        assert_eq!(read.text.as_deref(), Some("second"));
    }

    #[test]
    fn register_template_adds_template_definition() {
        let mut registry = ResourceRegistry::new();
        registry.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "task-template".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "ok"))),
        );

        assert_eq!(registry.list_templates().len(), 1);
        assert_eq!(registry.list_templates()[0].name, "task-template");
    }

    #[test]
    fn register_template_replaces_existing_template() {
        let mut registry = ResourceRegistry::new();
        registry.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "first".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "first"))),
        );
        registry.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "second".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "second"))),
        );

        assert_eq!(registry.list_templates().len(), 1);
        assert_eq!(registry.list_templates()[0].name, "second");
    }

    #[test]
    fn read_resource_uses_exact_handler_first() {
        let mut registry = ResourceRegistry::new();
        registry.register_resource(
            ResourceDefinition {
                uri: "othala://tasks/42".to_string(),
                name: "exact".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "exact"))),
        );
        registry.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "template".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "template"))),
        );

        let content = registry
            .read_resource("othala://tasks/42")
            .expect("should resolve exact handler");
        assert_eq!(content.text.as_deref(), Some("exact"));
    }

    #[test]
    fn read_resource_resolves_template_handler() {
        let mut registry = ResourceRegistry::new();
        registry.register_template(
            ResourceTemplate {
                uri_template: "othala://tasks/{id}".to_string(),
                name: "template".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|uri| Ok(text_content(uri, "templated"))),
        );

        let content = registry
            .read_resource("othala://tasks/abc")
            .expect("template should match");
        assert_eq!(content.uri, "othala://tasks/abc");
        assert_eq!(content.text.as_deref(), Some("templated"));
    }

    #[test]
    fn read_resource_rejects_invalid_uri() {
        let registry = ResourceRegistry::new();
        let result = registry.read_resource("https://tasks");
        assert_eq!(
            result.expect_err("must reject non-othala URI"),
            ResourceError::InvalidUri("https://tasks".to_string())
        );
    }

    #[test]
    fn read_resource_returns_not_found() {
        let registry = ResourceRegistry::new();
        let result = registry.read_resource("othala://missing");
        assert_eq!(
            result.expect_err("missing resource must fail"),
            ResourceError::NotFound("othala://missing".to_string())
        );
    }

    #[test]
    fn read_resource_wraps_non_read_failed_errors() {
        let mut registry = ResourceRegistry::new();
        registry.register_resource(
            ResourceDefinition {
                uri: "othala://broken".to_string(),
                name: "broken".to_string(),
                description: None,
                mime_type: None,
            },
            Box::new(|_| Err(ResourceError::InvalidUri("bad".to_string()))),
        );

        let err = registry
            .read_resource("othala://broken")
            .expect_err("read must fail");
        assert_eq!(err, ResourceError::ReadFailed("invalid resource uri: bad".to_string()));
    }

    #[test]
    fn register_builtin_resources_adds_expected_uris() {
        let mut registry = ResourceRegistry::new();
        registry.register_builtin_resources();

        let uris = registry
            .list_resources()
            .iter()
            .map(|resource| resource.uri.as_str())
            .collect::<Vec<_>>();

        assert_eq!(uris.len(), 7);
        assert!(uris.contains(&"othala://tasks"));
        assert!(uris.contains(&"othala://config"));
        assert!(uris.contains(&"othala://events"));
        assert!(uris.contains(&"othala://sessions"));
        assert!(uris.contains(&"othala://health"));
        assert!(uris.contains(&"othala://skills"));
        assert!(uris.contains(&"othala://stats"));
    }

    #[test]
    fn register_builtin_resources_supports_task_template() {
        let mut registry = ResourceRegistry::new();
        registry.register_builtin_resources();

        let content = registry
            .read_resource("othala://tasks/17")
            .expect("task template should resolve");
        assert_eq!(content.mime_type.as_deref(), Some("application/json"));
        assert!(
            content
                .text
                .expect("content should include text")
                .contains("othala://tasks/17")
        );
    }

    #[test]
    fn prompt_role_display_is_lowercase() {
        assert_eq!(PromptRole::User.to_string(), "user");
        assert_eq!(PromptRole::Assistant.to_string(), "assistant");
    }

    #[test]
    fn prompt_registry_register_and_list() {
        let mut registry = PromptRegistry::new();
        registry.register_prompt(
            PromptDefinition {
                name: "ad-hoc".to_string(),
                description: None,
                arguments: vec![],
            },
            Box::new(|_| {
                Ok(vec![PromptMessage {
                    role: PromptRole::User,
                    content: PromptContent::Text {
                        text: "hello".to_string(),
                    },
                }])
            }),
        );

        assert_eq!(registry.list_prompts().len(), 1);
        assert_eq!(registry.list_prompts()[0].name, "ad-hoc");
    }

    #[test]
    fn prompt_registry_replaces_existing_prompt_definition() {
        let mut registry = PromptRegistry::new();
        registry.register_prompt(
            PromptDefinition {
                name: "replace".to_string(),
                description: Some("first".to_string()),
                arguments: vec![],
            },
            Box::new(|_| Ok(vec![])),
        );
        registry.register_prompt(
            PromptDefinition {
                name: "replace".to_string(),
                description: Some("second".to_string()),
                arguments: vec![],
            },
            Box::new(|_| Ok(vec![])),
        );

        assert_eq!(registry.list_prompts().len(), 1);
        assert_eq!(registry.list_prompts()[0].description.as_deref(), Some("second"));
    }

    #[test]
    fn get_prompt_rejects_unknown_prompt() {
        let registry = PromptRegistry::new();
        let err = registry
            .get_prompt("missing", HashMap::new())
            .expect_err("unknown prompts should fail");
        assert_eq!(err, ResourceError::PromptNotFound("missing".to_string()));
    }

    #[test]
    fn get_prompt_validates_required_arguments() {
        let mut registry = PromptRegistry::new();
        registry.register_prompt(
            PromptDefinition {
                name: "needs-arg".to_string(),
                description: None,
                arguments: vec![PromptArgument {
                    name: "topic".to_string(),
                    description: None,
                    required: true,
                }],
            },
            Box::new(|_| Ok(vec![])),
        );

        let err = registry
            .get_prompt("needs-arg", HashMap::new())
            .expect_err("required arg missing");
        assert_eq!(
            err,
            ResourceError::InvalidArguments("missing required argument 'topic'".to_string())
        );
    }

    #[test]
    fn get_prompt_runs_handler_with_args() {
        let mut registry = PromptRegistry::new();
        registry.register_prompt(
            PromptDefinition {
                name: "echo".to_string(),
                description: None,
                arguments: vec![PromptArgument {
                    name: "value".to_string(),
                    description: None,
                    required: true,
                }],
            },
            Box::new(|args| {
                let value = args.get("value").cloned().unwrap_or_default();
                Ok(vec![PromptMessage {
                    role: PromptRole::Assistant,
                    content: PromptContent::Text {
                        text: format!("echo:{}", value),
                    },
                }])
            }),
        );

        let mut args = HashMap::new();
        args.insert("value".to_string(), "ping".to_string());
        let messages = registry
            .get_prompt("echo", args)
            .expect("prompt handler should run");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, PromptRole::Assistant);
        assert_eq!(
            messages[0].content,
            PromptContent::Text {
                text: "echo:ping".to_string()
            }
        );
    }

    #[test]
    fn register_builtin_prompts_adds_all_expected_prompt_names() {
        let mut registry = PromptRegistry::new();
        registry.register_builtin_prompts();

        let names = registry
            .list_prompts()
            .iter()
            .map(|prompt| prompt.name.as_str())
            .collect::<Vec<_>>();

        assert_eq!(names.len(), 5);
        assert!(names.contains(&"code-review"));
        assert!(names.contains(&"bug-fix"));
        assert!(names.contains(&"test-generation"));
        assert!(names.contains(&"refactor"));
        assert!(names.contains(&"explain"));
    }

    #[test]
    fn builtin_prompt_returns_messages() {
        let mut registry = PromptRegistry::new();
        registry.register_builtin_prompts();

        let mut args = HashMap::new();
        args.insert("topic".to_string(), "resource registry".to_string());
        let messages = registry
            .get_prompt("explain", args)
            .expect("builtin prompt should produce messages");

        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, PromptRole::User);
    }

    #[test]
    fn prompt_content_resource_serializes() {
        let message = PromptMessage {
            role: PromptRole::Assistant,
            content: PromptContent::Resource {
                resource: ResourceContent {
                    uri: "othala://tasks".to_string(),
                    mime_type: Some("application/json".to_string()),
                    text: Some("{}".to_string()),
                    blob: None,
                },
            },
        };

        let encoded = serde_json::to_string(&message).expect("serialize prompt message");
        assert!(encoded.contains("\"role\":\"assistant\""));
        assert!(encoded.contains("\"type\":\"resource\""));
    }

    #[test]
    fn uri_template_matching_accepts_placeholder_segments() {
        assert!(uri_matches_template(
            "othala://tasks/{id}",
            "othala://tasks/123"
        ));
        assert!(uri_matches_template(
            "othala://events/{date}/raw",
            "othala://events/2026-01-01/raw"
        ));
    }

    #[test]
    fn uri_template_matching_rejects_shape_mismatch() {
        assert!(!uri_matches_template(
            "othala://tasks/{id}",
            "othala://tasks"
        ));
        assert!(!uri_matches_template(
            "othala://tasks/{id}",
            "othala://events/123"
        ));
    }

    #[test]
    fn resource_error_display_messages_are_readable() {
        assert_eq!(
            ResourceError::NotFound("x".to_string()).to_string(),
            "resource not found: x"
        );
        assert_eq!(
            ResourceError::PromptNotFound("y".to_string()).to_string(),
            "prompt not found: y"
        );
    }
}

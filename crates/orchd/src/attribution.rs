//! Attribution system for AI-generated commits and PRs.

use serde::{Deserialize, Serialize};

/// Attribution style for AI-generated content
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttributionStyle {
    /// "Assisted-by: Model via Othala <othala@noreply>"
    #[default]
    AssistedBy,
    /// "Co-Authored-By: Othala <othala@noreply>"
    CoAuthoredBy,
    /// No attribution
    None,
}

impl AttributionStyle {
    pub fn as_str(&self) -> &str {
        match self {
            AttributionStyle::AssistedBy => "assisted-by",
            AttributionStyle::CoAuthoredBy => "co-authored-by",
            AttributionStyle::None => "none",
        }
    }
}

impl std::str::FromStr for AttributionStyle {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "assisted-by" | "assisted_by" => Ok(AttributionStyle::AssistedBy),
            "co-authored-by" | "co_authored_by" | "coauthored" => {
                Ok(AttributionStyle::CoAuthoredBy)
            }
            "none" | "off" | "disabled" => Ok(AttributionStyle::None),
            other => Err(format!(
                "invalid attribution style: '{other}'. valid: assisted-by, co-authored-by, none"
            )),
        }
    }
}

impl std::fmt::Display for AttributionStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration for attribution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttributionConfig {
    /// Attribution style for git commits
    pub commit_style: AttributionStyle,
    /// Attribution style for PR descriptions
    pub pr_style: AttributionStyle,
    /// Custom tool name (default: "Othala")
    pub tool_name: String,
    /// Custom email (default: "othala@noreply")
    pub tool_email: String,
    /// Add "Generated with {tool}" line to PR descriptions
    pub add_generated_line: bool,
}

impl Default for AttributionConfig {
    fn default() -> Self {
        Self {
            commit_style: AttributionStyle::AssistedBy,
            pr_style: AttributionStyle::AssistedBy,
            tool_name: "Othala".to_string(),
            tool_email: "othala@noreply".to_string(),
            add_generated_line: true,
        }
    }
}

impl AttributionConfig {
    /// Generate the commit trailer line
    pub fn commit_trailer(&self, model_name: &str) -> Option<String> {
        match &self.commit_style {
            AttributionStyle::AssistedBy => Some(format!(
                "Assisted-by: {} via {} <{}>",
                model_name, self.tool_name, self.tool_email
            )),
            AttributionStyle::CoAuthoredBy => {
                Some(format!("Co-Authored-By: {} <{}>", self.tool_name, self.tool_email))
            }
            AttributionStyle::None => None,
        }
    }

    /// Generate the PR attribution text
    pub fn pr_attribution(&self, model_name: &str) -> Option<String> {
        let mut parts = Vec::new();

        match &self.pr_style {
            AttributionStyle::AssistedBy => {
                parts.push(format!("Assisted by {} via {}", model_name, self.tool_name));
            }
            AttributionStyle::CoAuthoredBy => {
                parts.push(format!("Co-authored with {}", self.tool_name));
            }
            AttributionStyle::None => {}
        }

        if self.add_generated_line {
            parts.push(format!("Generated with {}", self.tool_name));
        }

        if parts.is_empty() {
            None
        } else {
            Some(parts.join("\n"))
        }
    }

    /// Append trailer to a commit message
    pub fn annotate_commit_message(&self, message: &str, model_name: &str) -> String {
        if let Some(trailer) = self.commit_trailer(model_name) {
            format!("{}\n\n{}", message.trim_end(), trailer)
        } else {
            message.to_string()
        }
    }

    /// Append attribution to a PR body
    pub fn annotate_pr_body(&self, body: &str, model_name: &str) -> String {
        if let Some(attr) = self.pr_attribution(model_name) {
            format!("{}\n\n---\n{}", body.trim_end(), attr)
        } else {
            body.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AttributionConfig, AttributionStyle};
    use std::str::FromStr;

    #[test]
    fn default_attribution_style() {
        assert_eq!(AttributionStyle::default(), AttributionStyle::AssistedBy);
    }

    #[test]
    fn commit_trailer_assisted_by_format() {
        let config = AttributionConfig::default();
        let trailer = config.commit_trailer("codex");

        assert_eq!(
            trailer,
            Some("Assisted-by: codex via Othala <othala@noreply>".to_string())
        );
    }

    #[test]
    fn commit_trailer_co_authored_by_format() {
        let mut config = AttributionConfig::default();
        config.commit_style = AttributionStyle::CoAuthoredBy;
        let trailer = config.commit_trailer("ignored");

        assert_eq!(trailer, Some("Co-Authored-By: Othala <othala@noreply>".to_string()));
    }

    #[test]
    fn commit_trailer_none_returns_none() {
        let mut config = AttributionConfig::default();
        config.commit_style = AttributionStyle::None;

        assert_eq!(config.commit_trailer("codex"), None);
    }

    #[test]
    fn pr_attribution_with_generated_line() {
        let config = AttributionConfig::default();
        let attr = config.pr_attribution("claude").expect("attribution present");

        assert!(attr.contains("Assisted by claude via Othala"));
        assert!(attr.contains("Generated with Othala"));
    }

    #[test]
    fn annotate_commit_message_appends_trailer() {
        let config = AttributionConfig::default();
        let annotated = config.annotate_commit_message("feat: improve pipeline", "gemini");

        assert!(annotated.starts_with("feat: improve pipeline\n\n"));
        assert!(annotated.contains("Assisted-by: gemini via Othala <othala@noreply>"));
    }

    #[test]
    fn annotate_pr_body_appends_attribution() {
        let config = AttributionConfig::default();
        let annotated = config.annotate_pr_body("## Summary\n- change", "codex");

        assert!(annotated.contains("## Summary\n- change\n\n---\n"));
        assert!(annotated.contains("Assisted by codex via Othala"));
        assert!(annotated.contains("Generated with Othala"));
    }

    #[test]
    fn attribution_style_from_str_parsing() {
        assert_eq!(
            AttributionStyle::from_str("assisted_by").expect("assisted_by parses"),
            AttributionStyle::AssistedBy
        );
        assert_eq!(
            AttributionStyle::from_str("co-authored-by").expect("co-authored-by parses"),
            AttributionStyle::CoAuthoredBy
        );
        assert_eq!(
            AttributionStyle::from_str("disabled").expect("disabled parses"),
            AttributionStyle::None
        );
        assert!(AttributionStyle::from_str("invalid-style").is_err());
    }

    #[test]
    fn attribution_style_display() {
        assert_eq!(AttributionStyle::AssistedBy.to_string(), "assisted-by");
        assert_eq!(AttributionStyle::CoAuthoredBy.to_string(), "co-authored-by");
        assert_eq!(AttributionStyle::None.to_string(), "none");
    }
}

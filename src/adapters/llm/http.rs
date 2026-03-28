use std::collections::BTreeMap;
use std::convert::Infallible;
use std::env;
use std::fmt;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use url::Url;

use crate::domains::cpg::Language;
use crate::domains::diagnostics::{LlmSuggestion, LlmSuggestionBundle, PatternType};
use crate::domains::Severity;
use crate::ports::llm::{LlmPort, LlmRequest};

const DEFAULT_PROVIDER: &str = "openai";
const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";
const DEFAULT_MODEL: &str = "gpt-4o-mini";
const AGGREGATE_BUDGET: Duration = Duration::from_secs(120);
const PER_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone, PartialEq, Eq)]
pub struct LlmConfig {
    pub provider: String,
    pub endpoint_url: String,
    pub api_key: String,
}

impl fmt::Debug for LlmConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LlmConfig")
            .field("provider", &self.provider)
            .field("endpoint_url", &self.endpoint_url)
            .field("api_key", &"***")
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LlmConfigError {
    MissingApiKey,
    UnsupportedProvider(String),
    InvalidEndpointUrl(String),
}

impl fmt::Display for LlmConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingApiKey => {
                write!(f, "`--llm` requires KALOS_LLM_API_KEY to be set")
            }
            Self::UnsupportedProvider(provider) => write!(
                f,
                "unsupported KALOS_LLM_PROVIDER `{provider}`; supported providers: openai"
            ),
            Self::InvalidEndpointUrl(url) => {
                write!(f, "KALOS_LLM_ENDPOINT_URL is not a valid URL: `{url}`")
            }
        }
    }
}

impl std::error::Error for LlmConfigError {}

pub fn validate_llm_config() -> Result<LlmConfig, LlmConfigError> {
    validate_llm_config_with(|key| env::var(key).ok())
}

fn validate_llm_config_with<F>(get_var: F) -> Result<LlmConfig, LlmConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    let provider = get_var("KALOS_LLM_PROVIDER").unwrap_or_else(|| DEFAULT_PROVIDER.to_owned());
    if provider != DEFAULT_PROVIDER {
        return Err(LlmConfigError::UnsupportedProvider(provider));
    }

    let endpoint_url = get_var("KALOS_LLM_ENDPOINT_URL")
        .unwrap_or_else(|| DEFAULT_OPENAI_ENDPOINT.to_owned());
    if Url::parse(&endpoint_url).is_err() {
        return Err(LlmConfigError::InvalidEndpointUrl(endpoint_url));
    }

    let Some(api_key) = get_var("KALOS_LLM_API_KEY").filter(|value| !value.trim().is_empty())
    else {
        return Err(LlmConfigError::MissingApiKey);
    };

    Ok(LlmConfig {
        provider,
        endpoint_url,
        api_key,
    })
}

fn sanitize_url_for_log(url: &str) -> String {
    let Ok(parsed) = Url::parse(url) else {
        return "<invalid-url>".to_owned();
    };
    let Some(host) = parsed.host_str() else {
        return "<invalid-url>".to_owned();
    };

    let mut sanitized = format!("{}://{}", parsed.scheme(), host);
    if let Some(port) = parsed.port() {
        sanitized.push(':');
        sanitized.push_str(&port.to_string());
    }
    sanitized.push_str(parsed.path());
    sanitized
}

#[derive(Clone, Debug)]
pub struct HttpLlmAdapter {
    config: LlmConfig,
}

impl HttpLlmAdapter {
    pub fn new(config: LlmConfig) -> Self {
        Self { config }
    }
}

impl LlmPort for HttpLlmAdapter {
    type Error = Infallible;

    fn enrich(&self, requests: &[LlmRequest]) -> Result<LlmSuggestionBundle, Self::Error> {
        if requests.is_empty() {
            return Ok(LlmSuggestionBundle {
                enrichments: BTreeMap::new(),
            });
        }

        eprintln!(
            "info: using llm endpoint {}",
            sanitize_url_for_log(&self.config.endpoint_url)
        );

        let started_at = Instant::now();
        let mut enrichments = BTreeMap::new();

        for request in requests {
            if remaining_budget(started_at).is_none() {
                eprintln!("warning: llm aggregate budget exhausted; skipping remaining diagnostics");
                break;
            }

            let Some(suggestion) = self.dispatch_request(request, started_at) else {
                continue;
            };
            enrichments.insert(request.diagnostic_id.clone(), suggestion);
        }

        Ok(LlmSuggestionBundle { enrichments })
    }
}

impl HttpLlmAdapter {
    fn dispatch_request(
        &self,
        request: &LlmRequest,
        started_at: Instant,
    ) -> Option<LlmSuggestion> {
        let mut attempt = 0_u8;

        while attempt < 2 {
            let timeout = PER_REQUEST_TIMEOUT.min(remaining_budget(started_at)?);
            let response = self.send_chat_completion(request, timeout);
            let Ok(mut response) = response else {
                return None;
            };

            match response.status().as_u16() {
                200 => {
                    let body = response.body_mut().read_to_string().ok()?;
                    let content = extract_message_content(&body)?;
                    return parse_suggestion_content(&content);
                }
                429 if attempt == 0 => {
                    let retry_after = response
                        .headers()
                        .get("retry-after")
                        .and_then(parse_retry_after)
                        .unwrap_or(Duration::ZERO)
                        .min(remaining_budget(started_at).unwrap_or(Duration::ZERO));
                    if retry_after > Duration::ZERO {
                        thread::sleep(retry_after);
                    }
                    attempt += 1;
                }
                500..=599 => return None,
                _ => return None,
            }
        }

        None
    }

    fn send_chat_completion(
        &self,
        request: &LlmRequest,
        timeout: Duration,
    ) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT.min(timeout)))
            .timeout_global(Some(timeout))
            .build()
            .into();

        let endpoint = format!(
            "{}/chat/completions",
            self.config.endpoint_url.trim_end_matches('/')
        );
        let body = json!({
            "model": DEFAULT_MODEL,
            "messages": [
                {
                    "role": "system",
                    "content": "You generate concise remediation guidance for a Kalos diagnostic. Return a short explanation. Include at most one fenced code block only when it materially helps."
                },
                {
                    "role": "user",
                    "content": build_prompt(request),
                }
            ]
        });
        let payload = serde_json::to_string(&body).expect("chat completion payload should serialize");

        agent
            .post(&endpoint)
            .header("authorization", format!("Bearer {}", self.config.api_key))
            .header("content-type", "application/json")
            .send(payload)
    }
}

fn remaining_budget(started_at: Instant) -> Option<Duration> {
    AGGREGATE_BUDGET.checked_sub(started_at.elapsed())
}

fn build_prompt(request: &LlmRequest) -> String {
    let mut prompt = format!(
        "rule_id: {}\nseverity: {}\nlanguage: {}\nworkspace_relative_path: {}\n",
        request.rule_id,
        severity_name(request.severity),
        language_name(request.language),
        request.workspace_relative_path,
    );

    if let Some(metric) = &request.metric {
        prompt.push_str(&format!(
            "metric: id={} raw_value={:.6} normalized_risk={:.6} threshold={:.6} overflow_ratio={:.6}\n",
            metric.metric_id,
            metric.raw_value,
            metric.normalized_risk,
            metric.threshold,
            metric.overflow_ratio,
        ));
    }
    if let Some(pattern) = &request.pattern {
        prompt.push_str(&format!(
            "pattern: type={} evidence_message={}\n",
            pattern_type_name(pattern.pattern_type),
            pattern.evidence_message,
        ));
        if !pattern.evidence_scopes.is_empty() {
            prompt.push_str("pattern_scopes:\n");
            for scope in &pattern.evidence_scopes {
                prompt.push_str(&format!(
                    "- level={:?} qualified_name={} file_path={}\n",
                    scope.level,
                    scope.qualified_name,
                    scope.file_path,
                ));
            }
        }
    }
    if let Some(source_excerpt) = &request.source_excerpt {
        prompt.push_str(&format!(
            "source_excerpt: file={} lines={}-{}\n{}\n",
            source_excerpt.file_path,
            source_excerpt.start_line,
            source_excerpt.end_line,
            source_excerpt.text,
        ));
    }
    if let Some(cpg_excerpt) = &request.cpg_excerpt {
        prompt.push_str("cpg_excerpt:\n");
        for scope in &cpg_excerpt.scopes {
            prompt.push_str(&format!(
                "- level={:?} qualified_name={} file_path={}\n",
                scope.level,
                scope.qualified_name,
                scope.file_path,
            ));
        }
        prompt.push_str(&cpg_excerpt.representation);
        prompt.push('\n');
    }

    prompt
}

fn extract_message_content(body: &str) -> Option<String> {
    let parsed = serde_json::from_str::<Value>(body).ok()?;
    let content = parsed
        .get("choices")?
        .as_array()?
        .first()?
        .get("message")?
        .get("content")?;

    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text = parts
                .iter()
                .filter_map(|part| {
                    (part.get("type")?.as_str()? == "text")
                        .then(|| part.get("text")?.as_str().map(str::to_owned))
                        .flatten()
                })
                .collect::<Vec<_>>()
                .join("\n");
            (!text.trim().is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn parse_suggestion_content(content: &str) -> Option<LlmSuggestion> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return None;
    }

    let Some(start) = trimmed.find("```") else {
        return Some(LlmSuggestion {
            explanation: trimmed.to_owned(),
            code_example: None,
        });
    };

    let fence_body = &trimmed[start + 3..];
    let Some(line_break) = fence_body.find('\n') else {
        return Some(LlmSuggestion {
            explanation: trimmed.to_owned(),
            code_example: None,
        });
    };
    let code_body = &fence_body[line_break + 1..];
    let Some(end) = code_body.find("```") else {
        return Some(LlmSuggestion {
            explanation: trimmed.to_owned(),
            code_example: None,
        });
    };

    let code_example = code_body[..end].trim().to_owned();
    let before = trimmed[..start].trim();
    let after = code_body[end + 3..].trim();
    let explanation = [before, after]
        .into_iter()
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");

    Some(LlmSuggestion {
        explanation,
        code_example: (!code_example.is_empty()).then_some(code_example),
    })
}

fn parse_retry_after(value: &ureq::http::HeaderValue) -> Option<Duration> {
    value
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "error",
        Severity::Warning => "warning",
        Severity::Info => "info",
    }
}

fn language_name(language: Language) -> &'static str {
    match language {
        Language::Python => "python",
        Language::TypeScript => "typescript",
        Language::Rust => "rust",
        Language::Go => "go",
    }
}

fn pattern_type_name(pattern_type: PatternType) -> &'static str {
    match pattern_type {
        PatternType::GodUnit => "god_unit",
        PatternType::FeatureEnvy => "feature_envy",
        PatternType::CircularDependency => "circular_dependency",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LlmConfig, LlmConfigError, extract_message_content, parse_suggestion_content,
        sanitize_url_for_log, validate_llm_config_with,
    };

    #[test]
    fn validate_llm_config_requires_api_key() {
        let error =
            validate_llm_config_with(|_| None).expect_err("missing api key should fail");

        assert_eq!(error, LlmConfigError::MissingApiKey);
        assert_eq!(
            error.to_string(),
            "`--llm` requires KALOS_LLM_API_KEY to be set"
        );
    }

    #[test]
    fn validate_llm_config_rejects_unsupported_provider() {
        let error = validate_llm_config_with(|key| match key {
            "KALOS_LLM_PROVIDER" => Some("anthropic".to_owned()),
            "KALOS_LLM_API_KEY" => Some("secret".to_owned()),
            _ => None,
        })
        .expect_err("unsupported provider should fail");

        assert_eq!(error, LlmConfigError::UnsupportedProvider("anthropic".to_owned()));
    }

    #[test]
    fn validate_llm_config_rejects_invalid_endpoint_url() {
        let error = validate_llm_config_with(|key| match key {
            "KALOS_LLM_API_KEY" => Some("secret".to_owned()),
            "KALOS_LLM_ENDPOINT_URL" => Some("not a url".to_owned()),
            _ => None,
        })
        .expect_err("invalid endpoint should fail");

        assert_eq!(error, LlmConfigError::InvalidEndpointUrl("not a url".to_owned()));
    }

    #[test]
    fn validate_llm_config_uses_openai_defaults() {
        let config = validate_llm_config_with(|key| match key {
            "KALOS_LLM_API_KEY" => Some("secret".to_owned()),
            _ => None,
        })
        .expect("default config should be valid");

        assert_eq!(
            config,
            LlmConfig {
                provider: "openai".to_owned(),
                endpoint_url: "https://api.openai.com/v1".to_owned(),
                api_key: "secret".to_owned(),
            }
        );
    }

    #[test]
    fn llm_config_debug_masks_api_key() {
        let config = LlmConfig {
            provider: "openai".to_owned(),
            endpoint_url: "https://api.openai.com/v1".to_owned(),
            api_key: "secret".to_owned(),
        };

        let debug = format!("{config:?}");

        assert!(debug.contains("provider: \"openai\""));
        assert!(debug.contains("endpoint_url: \"https://api.openai.com/v1\""));
        assert!(debug.contains("api_key: \"***\""));
        assert!(!debug.contains("secret"));
    }

    #[test]
    fn sanitize_url_for_log_strips_query_and_fragment() {
        assert_eq!(
            sanitize_url_for_log("https://api.example.com/v1/chat/completions?token=secret#frag"),
            "https://api.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn sanitize_url_for_log_returns_invalid_placeholder() {
        assert_eq!(sanitize_url_for_log("://bad"), "<invalid-url>");
    }

    #[test]
    fn extracts_message_content_from_chat_completion_response() {
        let body = r#"{"choices":[{"message":{"content":"explanation"}}]}"#;

        assert_eq!(extract_message_content(body).as_deref(), Some("explanation"));
    }

    #[test]
    fn parses_code_block_into_code_example() {
        let suggestion = parse_suggestion_content(
            "Refactor the branch handling.\n\n```rust\nfn helper() {}\n```\n\nKeep the public API small.",
        )
        .expect("suggestion should parse");

        assert_eq!(
            suggestion.explanation,
            "Refactor the branch handling.\n\nKeep the public API small."
        );
        assert_eq!(suggestion.code_example.as_deref(), Some("fn helper() {}"));
    }
}

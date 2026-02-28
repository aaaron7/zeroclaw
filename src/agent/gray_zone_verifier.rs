use crate::providers::Provider;
use async_trait::async_trait;
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone)]
pub struct GrayZoneVerificationRequest<'a> {
    pub provider: &'a dyn Provider,
    pub model: &'a str,
    pub original_request: &'a str,
    pub model_response: &'a str,
    pub continue_reason: &'a str,
    pub missing_requirements: &'a [String],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GrayZoneVerdict {
    pub done: bool,
    pub reason: String,
}

#[async_trait]
pub trait GrayZoneVerifier: Send + Sync {
    async fn verify(
        &self,
        request: GrayZoneVerificationRequest<'_>,
    ) -> anyhow::Result<GrayZoneVerdict>;
}

#[derive(Debug, Clone)]
pub struct ProviderGrayZoneVerifier {
    timeout: Duration,
}

impl ProviderGrayZoneVerifier {
    pub fn new(timeout_ms: u64) -> Self {
        Self {
            timeout: Duration::from_millis(timeout_ms),
        }
    }
}

#[async_trait]
impl GrayZoneVerifier for ProviderGrayZoneVerifier {
    async fn verify(
        &self,
        request: GrayZoneVerificationRequest<'_>,
    ) -> anyhow::Result<GrayZoneVerdict> {
        let system_prompt = "You are a strict task completion verifier. Return JSON only: {\"done\": boolean, \"reason\": string}. Use done=true only when current output can be treated as finished without any additional tool execution.";

        let user_prompt = format!(
            "original_request:\n{}\n\nmodel_response:\n{}\n\ncontinue_reason:\n{}\n\nmissing_requirements:\n{}\n\nReturn JSON only.",
            request.original_request,
            request.model_response,
            request.continue_reason,
            serde_json::to_string(request.missing_requirements).unwrap_or_else(|_| "[]".to_string()),
        );

        let raw = tokio::time::timeout(
            self.timeout,
            request.provider.chat_with_system(
                Some(system_prompt),
                &user_prompt,
                request.model,
                0.0,
            ),
        )
        .await
        .map_err(|_| anyhow::anyhow!("gray-zone verifier timed out"))??;

        parse_gray_zone_verdict(&raw)
    }
}

#[derive(Debug, Deserialize)]
struct GrayZoneVerdictPayload {
    done: bool,
    reason: String,
}

fn parse_gray_zone_verdict(raw: &str) -> anyhow::Result<GrayZoneVerdict> {
    let trimmed = raw.trim();
    let payload_json = extract_json_object(trimmed).unwrap_or(trimmed);
    let parsed: GrayZoneVerdictPayload = serde_json::from_str(payload_json)
        .map_err(|e| anyhow::anyhow!("invalid gray-zone verifier payload: {e}"))?;

    Ok(GrayZoneVerdict {
        done: parsed.done,
        reason: parsed.reason,
    })
}

fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end <= start {
        return None;
    }
    Some(&text[start..=end])
}

#[cfg(test)]
mod tests {
    use super::{GrayZoneVerificationRequest, GrayZoneVerifier, ProviderGrayZoneVerifier};
    use crate::providers::Provider;
    use async_trait::async_trait;
    use std::sync::Mutex;
    use std::time::Duration;

    struct ScriptedProvider {
        responses: Mutex<Vec<anyhow::Result<String>>>,
        delay: Duration,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<anyhow::Result<String>>, delay: Duration) -> Self {
            Self {
                responses: Mutex::new(responses),
                delay,
            }
        }
    }

    #[async_trait]
    impl Provider for ScriptedProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            _message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let mut guard = self.responses.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                return Ok(r#"{"done":false,"reason":"empty_script"}"#.to_string());
            }
            guard.remove(0)
        }
    }

    #[tokio::test]
    async fn gray_zone_verifier_parses_json_payload() {
        let provider = ScriptedProvider::new(
            vec![Ok(r#"{"done":true,"reason":"verified"}"#.to_string())],
            Duration::from_millis(0),
        );
        let verifier = ProviderGrayZoneVerifier::new(1500);
        let request = GrayZoneVerificationRequest {
            provider: &provider,
            model: "test-model",
            original_request: "继续",
            model_response: "我正在检查",
            continue_reason: "unknown_contract_non_terminal_update",
            missing_requirements: &[],
        };

        let verdict = verifier.verify(request).await.expect("verifier result");
        assert!(verdict.done);
        assert_eq!(verdict.reason, "verified");
    }

    #[tokio::test]
    async fn gray_zone_verifier_times_out_when_provider_is_slow() {
        let provider = ScriptedProvider::new(
            vec![Ok(r#"{"done":true,"reason":"too_late"}"#.to_string())],
            Duration::from_millis(80),
        );
        let verifier = ProviderGrayZoneVerifier::new(10);
        let request = GrayZoneVerificationRequest {
            provider: &provider,
            model: "test-model",
            original_request: "继续",
            model_response: "我正在检查",
            continue_reason: "unknown_contract_non_terminal_update",
            missing_requirements: &[],
        };

        let err = verifier
            .verify(request)
            .await
            .expect_err("timeout should fail");
        assert!(format!("{err:#}").contains("timed out"));
    }
}

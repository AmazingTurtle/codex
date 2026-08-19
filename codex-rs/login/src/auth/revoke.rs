//! Best-effort OAuth token revocation used during logout.
//!
//! Managed ChatGPT auth stores OAuth tokens locally. Logout attempts to revoke the
//! refresh token, falling back to the access token when no refresh token is
//! available, and callers still remove local auth if the revoke request fails.

use serde::Serialize;
use std::time::Duration;

use codex_http_client::HttpClient;
use codex_protocol::auth::AuthMode;

use super::manager::REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR;
use super::manager::REVOKE_TOKEN_URL;
use super::manager::REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR;
use super::manager::oauth_client_id;
use super::storage::AuthDotJson;
use super::util::try_parse_error_message;
use crate::default_client::create_default_auth_client;
use crate::outbound_proxy::AuthRouteConfig;
use crate::token_data::TokenData;

const REVOKE_HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const REVOKE_TOTAL_TIMEOUT: Duration = Duration::from_secs(10);
const REVOKE_MAX_CONCURRENCY: usize = 8;

#[derive(Clone, Copy)]
struct RevokeLimits {
    request_timeout: Duration,
    total_timeout: Duration,
    max_concurrency: usize,
}

impl Default for RevokeLimits {
    fn default() -> Self {
        Self {
            request_timeout: REVOKE_HTTP_TIMEOUT,
            total_timeout: REVOKE_TOTAL_TIMEOUT,
            max_concurrency: REVOKE_MAX_CONCURRENCY,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RevokeTokenKind {
    Access,
    Refresh,
}

impl RevokeTokenKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Access => "access_token",
            Self::Refresh => "refresh_token",
        }
    }

    fn client_id(self) -> Option<String> {
        match self {
            Self::Access => None,
            Self::Refresh => Some(oauth_client_id()),
        }
    }
}

#[derive(Serialize)]
struct RevokeTokenRequest<'a> {
    token: &'a str,
    token_type_hint: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    client_id: Option<String>,
}

pub(super) async fn revoke_auth_tokens(
    auth_dot_json: Option<&AuthDotJson>,
    auth_route_config: &AuthRouteConfig,
) -> Result<(), std::io::Error> {
    revoke_auth_tokens_with_limits(auth_dot_json, auth_route_config, RevokeLimits::default()).await
}

async fn revoke_auth_tokens_with_limits(
    auth_dot_json: Option<&AuthDotJson>,
    auth_route_config: &AuthRouteConfig,
    limits: RevokeLimits,
) -> Result<(), std::io::Error> {
    let Some(auth_dot_json) = auth_dot_json else {
        return Ok(());
    };

    let endpoint = revoke_token_endpoint();
    let client = create_default_auth_client(&endpoint, auth_route_config)?;
    let mut tokens = std::iter::once(auth_dot_json)
        .chain(&auth_dot_json.accounts)
        .filter_map(revocable_token)
        .map(|(token, kind)| (token.to_string(), kind));
    let max_concurrency = limits.max_concurrency.max(1);
    let mut revocations = tokio::task::JoinSet::new();
    for _ in 0..max_concurrency {
        let Some((token, kind)) = tokens.next() else {
            break;
        };
        let client = client.clone();
        let endpoint = endpoint.clone();
        revocations.spawn(async move {
            revoke_oauth_token(
                &client,
                endpoint.as_str(),
                &token,
                kind,
                limits.request_timeout,
            )
            .await
        });
    }

    let mut failures = Vec::new();
    let deadline = tokio::time::sleep(limits.total_timeout);
    tokio::pin!(deadline);
    while !revocations.is_empty() {
        tokio::select! {
            result = revocations.join_next() => {
                match result {
                    Some(Ok(Ok(()))) => {}
                    Some(Ok(Err(err))) => failures.push(err.to_string()),
                    Some(Err(err)) => failures.push(format!("token revocation task failed: {err}")),
                    None => break,
                }
                if let Some((token, kind)) = tokens.next() {
                    let client = client.clone();
                    let endpoint = endpoint.clone();
                    revocations.spawn(async move {
                        revoke_oauth_token(
                            &client,
                            endpoint.as_str(),
                            &token,
                            kind,
                            limits.request_timeout,
                        )
                        .await
                    });
                }
            }
            () = &mut deadline => {
                let remaining = revocations.len() + tokens.count();
                revocations.abort_all();
                failures.push(format!(
                    "timed out revoking {remaining} stored OAuth token(s) after {}s",
                    limits.total_timeout.as_secs_f64()
                ));
                break;
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(failures.join("; ")))
    }
}

fn revocable_token(auth_dot_json: &AuthDotJson) -> Option<(&str, RevokeTokenKind)> {
    let tokens = managed_chatgpt_tokens(auth_dot_json)?;
    if !tokens.refresh_token.is_empty() {
        Some((tokens.refresh_token.as_str(), RevokeTokenKind::Refresh))
    } else if !tokens.access_token.is_empty() {
        Some((tokens.access_token.as_str(), RevokeTokenKind::Access))
    } else {
        None
    }
}

fn managed_chatgpt_tokens(auth_dot_json: &AuthDotJson) -> Option<&TokenData> {
    if resolved_auth_mode(auth_dot_json) == AuthMode::Chatgpt {
        auth_dot_json.tokens.as_ref()
    } else {
        None
    }
}

fn resolved_auth_mode(auth_dot_json: &AuthDotJson) -> AuthMode {
    if let Some(mode) = auth_dot_json.auth_mode {
        return mode;
    }
    if auth_dot_json.openai_api_key.is_some() {
        return AuthMode::ApiKey;
    }
    AuthMode::Chatgpt
}

async fn revoke_oauth_token(
    client: &HttpClient,
    endpoint: &str,
    token: &str,
    kind: RevokeTokenKind,
    timeout: Duration,
) -> Result<(), std::io::Error> {
    let request = RevokeTokenRequest {
        token,
        token_type_hint: kind.as_str(),
        client_id: kind.client_id(),
    };

    let response = client
        .post(endpoint)
        .header("Content-Type", "application/json")
        .timeout(timeout)
        .json(&request)
        .send()
        .await
        .map_err(std::io::Error::other)?;

    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let body = response.text().await.unwrap_or_default();
    let message = try_parse_error_message(&body);
    Err(std::io::Error::other(format!(
        "failed to revoke {}: {}: {}",
        kind.as_str(),
        status,
        message
    )))
}

fn revoke_token_endpoint() -> String {
    if let Ok(endpoint) = std::env::var(REVOKE_TOKEN_URL_OVERRIDE_ENV_VAR) {
        return endpoint;
    }

    if let Ok(refresh_endpoint) = std::env::var(REFRESH_TOKEN_URL_OVERRIDE_ENV_VAR)
        && let Some(endpoint) = derive_revoke_token_endpoint(&refresh_endpoint)
    {
        return endpoint;
    }

    REVOKE_TOKEN_URL.to_string()
}

fn derive_revoke_token_endpoint(refresh_endpoint: &str) -> Option<String> {
    let mut url = url::Url::parse(refresh_endpoint).ok()?;
    url.set_path("/oauth/revoke");
    url.set_query(None);
    Some(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_http_client::ClientRouteClass;
    use codex_http_client::HttpClientFactory;
    use codex_http_client::OutboundProxyPolicy;
    use core_test_support::skip_if_no_network;
    use wiremock::Mock;
    use wiremock::MockServer;
    use wiremock::ResponseTemplate;
    use wiremock::matchers::method;
    use wiremock::matchers::path;

    #[test]
    fn derives_revoke_url_from_refresh_token_override() {
        assert_eq!(
            derive_revoke_token_endpoint("http://127.0.0.1:1234/oauth/token?unified=true"),
            Some("http://127.0.0.1:1234/oauth/revoke".to_string())
        );
    }

    #[tokio::test]
    async fn revoke_request_times_out() {
        skip_if_no_network!();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth/revoke"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
            .mount(&server)
            .await;

        let endpoint = format!("{}/oauth/revoke", server.uri());
        let client = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault)
            .build_client(&endpoint, ClientRouteClass::Auth)
            .expect("test HTTP client should build");
        let error = revoke_oauth_token(
            &client,
            endpoint.as_str(),
            "refresh-token",
            RevokeTokenKind::Refresh,
            Duration::from_millis(20),
        )
        .await
        .expect_err("stalled revoke request should time out");

        let reqwest_error = error
            .get_ref()
            .and_then(|error| error.downcast_ref::<codex_http_client::HttpError>())
            .expect("timeout error should preserve HTTP client error");
        assert!(reqwest_error.is_timeout());
    }
}

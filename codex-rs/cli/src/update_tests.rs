use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

#[test]
fn validates_downstream_release_tags() {
    for tag in ["v0.148.0-better-codex", "v0.148.0-better-codex.2"] {
        validate_release_tag(tag).expect("valid downstream tag");
    }
    for tag in [
        "0.148.0-better-codex",
        "v0.148-better-codex",
        "v0.148.0",
        "v0.148.0-better-codex.beta",
        "v0.148.0-better-codex.2.extra",
    ] {
        assert!(
            validate_release_tag(tag).is_err(),
            "unexpected valid tag: {tag}"
        );
    }
}

#[test]
fn renders_pinned_source_install_command() {
    assert_eq!(
        install_command("v0.148.0-better-codex"),
        "cargo install --git https://github.com/AmazingTurtle/codex --tag v0.148.0-better-codex --locked --force --bin better-codex codex-cli"
    );
}

#[tokio::test]
async fn fetches_and_validates_latest_release() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/releases/latest"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "tag_name": "v0.148.0-better-codex.1"
        })))
        .mount(&server)
        .await;

    let tag = fetch_latest_release_tag(
        &format!("{}/releases/latest", server.uri()),
        HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault),
    )
    .await
    .expect("latest release");

    assert_eq!(tag, "v0.148.0-better-codex.1");
}

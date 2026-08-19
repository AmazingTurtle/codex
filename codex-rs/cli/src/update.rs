use anyhow::Context;
use anyhow::Result;
use anyhow::ensure;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::RouteAwareClientPool;
use codex_login::default_client::default_headers;
use serde::Deserialize;
use std::time::Duration;

const LATEST_RELEASE_URL: &str = "https://api.github.com/repos/AmazingTurtle/codex/releases/latest";

#[derive(Deserialize)]
struct ReleaseInfo {
    tag_name: String,
}

pub(crate) async fn run() -> Result<()> {
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let tag = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        fetch_latest_release_tag(LATEST_RELEASE_URL, factory),
    )
    .await
    .context("timed out checking the latest Better Codex release")??;
    println!("Latest Better Codex release: {tag}");
    println!("{}", install_command(&tag));
    Ok(())
}

async fn fetch_latest_release_tag(url: &str, factory: HttpClientFactory) -> Result<String> {
    let client_pool = RouteAwareClientPool::new(factory, ClientRouteClass::Other)
        .with_legacy_custom_ca_fallback();
    let release = client_pool
        .get(url)
        .headers(default_headers())
        .send()
        .await
        .context("check the latest Better Codex release")?
        .error_for_status()
        .context("check the latest Better Codex release")?
        .json::<ReleaseInfo>()
        .await
        .context("parse the latest Better Codex release")?;
    validate_release_tag(&release.tag_name)?;
    Ok(release.tag_name)
}

fn validate_release_tag(tag: &str) -> Result<()> {
    let version = tag
        .strip_prefix('v')
        .context("Better Codex release tag must start with 'v'")?;
    let (base, revision) = version
        .split_once("-better-codex")
        .context("Better Codex release tag must contain '-better-codex'")?;
    let components = base.split('.').collect::<Vec<_>>();
    ensure!(
        components.len() == 3
            && components
                .iter()
                .all(|component| !component.is_empty() && component.parse::<u64>().is_ok()),
        "Better Codex release tag has an invalid base version: {tag}"
    );
    ensure!(
        revision.is_empty()
            || revision
                .strip_prefix('.')
                .is_some_and(|value| !value.is_empty() && value.parse::<u64>().is_ok()),
        "Better Codex release tag has an invalid revision: {tag}"
    );
    Ok(())
}

fn install_command(tag: &str) -> String {
    format!(
        "cargo install --git {} --tag {tag} --locked --force --bin {} codex-cli",
        codex_product_info::REPOSITORY,
        codex_product_info::CLI_NAME,
    )
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;

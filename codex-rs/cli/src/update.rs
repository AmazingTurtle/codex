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
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
}

enum InstallSource {
    Package,
    Source,
    LegacySource,
}

pub(crate) async fn run() -> Result<()> {
    let factory = HttpClientFactory::new(OutboundProxyPolicy::ReqwestDefault);
    let release = tokio::time::timeout(
        Duration::from_secs(/*secs*/ 10),
        fetch_latest_release(LATEST_RELEASE_URL, factory),
    )
    .await
    .context("timed out checking the latest Better Codex release")??;
    println!("Latest Better Codex release: {}", release.tag_name);
    let source = if cfg!(all(
        target_os = "linux",
        target_arch = "x86_64",
        target_env = "gnu"
    )) && release
        .assets
        .iter()
        .any(|asset| asset.name == "better-codex-x86_64-unknown-linux-gnu.tar.gz")
    {
        InstallSource::Package
    } else if release
        .assets
        .iter()
        .any(|asset| asset.name == "release.json")
    {
        InstallSource::Source
    } else {
        InstallSource::LegacySource
    };
    println!("{}", install_command(&release.tag_name, source));
    Ok(())
}

async fn fetch_latest_release(url: &str, factory: HttpClientFactory) -> Result<ReleaseInfo> {
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
    Ok(release)
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

fn install_command(tag: &str, source: InstallSource) -> String {
    match source {
        InstallSource::Package => format!(
            "curl -fsSL https://raw.githubusercontent.com/AmazingTurtle/codex/{tag}/scripts/better_codex_release/install.py | python3 - --version {tag}"
        ),
        InstallSource::Source => format!(
            "git clone --depth 1 --branch {tag} {} better-codex-source && cd better-codex-source && just assemble-codex-package",
            codex_product_info::REPOSITORY,
        ),
        InstallSource::LegacySource => format!(
            "cargo install --git {} --tag {tag} --locked --force --bin {} codex-cli",
            codex_product_info::REPOSITORY,
            codex_product_info::CLI_NAME,
        ),
    }
}

#[cfg(test)]
#[path = "update_tests.rs"]
mod tests;

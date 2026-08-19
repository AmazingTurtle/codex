pub(crate) fn is_newer(latest: &str, current: &str) -> Option<bool> {
    match (parse_version(latest), parse_version(current)) {
        (Some(l), Some(c)) => Some(l > c),
        _ => None,
    }
}

pub(crate) fn extract_version_from_latest_tag(latest_tag_name: &str) -> anyhow::Result<String> {
    latest_tag_name
        .strip_prefix('v')
        .filter(|version| version.contains("-better-codex"))
        .map(str::to_owned)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse latest tag name '{latest_tag_name}'"))
}

pub(crate) fn is_source_build_version(version: &str) -> bool {
    parse_version(version) == Some((0, 0, 0, 0))
}

fn parse_version(v: &str) -> Option<(u64, u64, u64, u64)> {
    let (base, downstream) = v.trim().split_once("-better-codex")?;
    let revision = match downstream {
        "" => 0,
        value => value.strip_prefix('.')?.parse::<u64>().ok()?,
    };
    let mut iter = base.split('.');
    let maj = iter.next()?.parse::<u64>().ok()?;
    let min = iter.next()?.parse::<u64>().ok()?;
    let pat = iter.next()?.parse::<u64>().ok()?;
    (iter.next().is_none()).then_some((maj, min, pat, revision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn extracts_version_from_latest_tag() {
        assert_eq!(
            extract_version_from_latest_tag("v1.5.0-better-codex")
                .expect("failed to parse version"),
            "1.5.0-better-codex"
        );
    }

    #[test]
    fn latest_tag_without_prefix_is_invalid() {
        assert!(extract_version_from_latest_tag("rust-v1.5.0").is_err());
    }

    #[test]
    fn prerelease_version_is_not_considered_newer() {
        assert_eq!(is_newer("0.11.0-beta.1", "0.11.0-better-codex"), None);
        assert_eq!(is_newer("1.0.0-rc.1", "1.0.0-better-codex"), None);
    }

    #[test]
    fn plain_semver_comparisons_work() {
        assert_eq!(
            is_newer("0.11.1-better-codex", "0.11.0-better-codex"),
            Some(true)
        );
        assert_eq!(
            is_newer("0.11.0-better-codex", "0.11.1-better-codex"),
            Some(false)
        );
        assert_eq!(
            is_newer("1.0.0-better-codex", "0.9.9-better-codex"),
            Some(true)
        );
        assert_eq!(
            is_newer("0.9.9-better-codex", "1.0.0-better-codex"),
            Some(false)
        );
        assert_eq!(
            is_newer("1.0.0-better-codex.1", "1.0.0-better-codex"),
            Some(true)
        );
    }

    #[test]
    fn source_build_version_is_not_checked() {
        assert!(is_source_build_version("0.0.0-better-codex"));
        assert!(!is_source_build_version("0.1.0-better-codex"));
    }

    #[test]
    fn whitespace_is_ignored() {
        assert_eq!(parse_version(" 1.2.3-better-codex \n"), Some((1, 2, 3, 0)));
        assert_eq!(
            is_newer(" 1.2.3-better-codex ", "1.2.2-better-codex"),
            Some(true)
        );
    }
}

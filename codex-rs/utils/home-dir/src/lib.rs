use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the path to the Better Codex configuration directory, which can be
/// specified by `BETTER_CODEX_HOME`. If not set, defaults to `~/.better-codex`.
///
/// - If `BETTER_CODEX_HOME` is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If `BETTER_CODEX_HOME` is not set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let codex_home_env = std::env::var(codex_product_info::HOME_ENV)
        .ok()
        .filter(|val| !val.is_empty());
    find_codex_home_from_env(
        codex_home_env.as_deref(),
        HomeRequirement::ExistingIfExplicit,
    )
}

/// Returns the path where a Better Codex configuration directory may be created.
///
/// Unlike [`find_codex_home`], an explicitly configured `BETTER_CODEX_HOME`
/// does not need to exist yet. Existing paths must still be directories.
pub fn find_codex_home_for_creation() -> std::io::Result<AbsolutePathBuf> {
    let codex_home_env = std::env::var(codex_product_info::HOME_ENV)
        .ok()
        .filter(|val| !val.is_empty());
    find_codex_home_from_env(codex_home_env.as_deref(), HomeRequirement::MayCreate)
}

#[derive(Clone, Copy)]
enum HomeRequirement {
    ExistingIfExplicit,
    MayCreate,
}

fn find_codex_home_from_env(
    codex_home_env: Option<&str>,
    requirement: HomeRequirement,
) -> std::io::Result<AbsolutePathBuf> {
    // Honor `BETTER_CODEX_HOME` when it is set to allow users
    // (and tests) to override the default location.
    match codex_home_env {
        Some(val) => {
            let path = PathBuf::from(val);
            let metadata = match std::fs::metadata(&path) {
                Ok(metadata) => Some(metadata),
                Err(err)
                    if err.kind() == std::io::ErrorKind::NotFound
                        && matches!(requirement, HomeRequirement::MayCreate) =>
                {
                    None
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::NotFound,
                        format!(
                            "BETTER_CODEX_HOME points to {val:?}, but that path does not exist"
                        ),
                    ));
                }
                Err(err) => {
                    return Err(std::io::Error::new(
                        err.kind(),
                        format!("failed to read BETTER_CODEX_HOME {val:?}: {err}"),
                    ));
                }
            };

            if metadata.as_ref().is_some_and(|metadata| !metadata.is_dir()) {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!(
                        "BETTER_CODEX_HOME points to {val:?}, but that path is not a directory"
                    ),
                ))
            } else if metadata.is_some() {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize BETTER_CODEX_HOME {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            } else {
                AbsolutePathBuf::from_absolute_path(std::path::absolute(path)?)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(codex_product_info::HOME_DIR_NAME);
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HomeRequirement;
    use super::find_codex_home_from_env;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_codex_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(missing_str), HomeRequirement::ExistingIfExplicit)
            .expect_err("missing BETTER_CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("BETTER_CODEX_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("codex-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(file_str), HomeRequirement::ExistingIfExplicit)
            .expect_err("file BETTER_CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp codex home path should be valid utf-8");

        let resolved =
            find_codex_home_from_env(Some(temp_str), HomeRequirement::ExistingIfExplicit)
                .expect("valid BETTER_CODEX_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_without_env_uses_default_home_dir() {
        let resolved = find_codex_home_from_env(
            /*codex_home_env*/ None,
            HomeRequirement::ExistingIfExplicit,
        )
        .expect("default BETTER_CODEX_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(codex_product_info::HOME_DIR_NAME);
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_for_creation_accepts_missing_path() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex home path should be valid utf-8");

        let resolved = find_codex_home_from_env(Some(missing_str), HomeRequirement::MayCreate)
            .expect("creatable BETTER_CODEX_HOME");
        let expected = AbsolutePathBuf::from_absolute_path(missing).expect("absolute home");
        assert_eq!(resolved, expected);
    }
}

use std::fs;
use std::path::Path;
use std::path::PathBuf;

use codex_config::version_for_bytes;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct HookCommandContentDigest {
    target: String,
    canonical_path: String,
    file_type: &'static str,
    permissions: String,
    len: u64,
    content_hash: String,
}

pub(crate) fn hook_command_content_digest(
    command_line: &str,
    cwd: &Path,
) -> Option<HookCommandContentDigest> {
    let target = command_content_target(command_line)?;
    if has_dynamic_path_syntax(target) {
        return None;
    }

    let target_path = Path::new(target);
    let resolved_path = if target_path.is_absolute() {
        PathBuf::from(target_path)
    } else {
        cwd.join(target_path)
    };
    let metadata = fs::symlink_metadata(&resolved_path).ok()?;
    let canonical_path = resolved_path.canonicalize().ok()?;
    let file_type = if metadata.file_type().is_symlink() {
        "symlink"
    } else if metadata.file_type().is_file() {
        "file"
    } else {
        return None;
    };
    let contents = fs::read(&resolved_path).ok()?;

    Some(HookCommandContentDigest {
        target: target.to_string(),
        canonical_path: canonical_path.display().to_string(),
        file_type,
        permissions: permissions_label(&metadata),
        len: metadata.len(),
        content_hash: version_for_bytes(&contents),
    })
}

fn command_content_target(command_line: &str) -> Option<&str> {
    let words = shell_words_prefix(command_line)?;
    let mut command_index = 0;
    while words
        .get(command_index)
        .is_some_and(|word| is_env_assignment(word))
    {
        command_index += 1;
    }

    let command = words.get(command_index)?;
    if is_path_like(command) {
        return Some(command);
    }

    let command_name = Path::new(command).file_name()?.to_str()?;
    if matches!(command_name, "sh" | "bash" | "dash" | "zsh" | "ksh") {
        return shell_script_arg(&words[(command_index + 1)..]);
    }

    if matches!(
        command_name,
        "python" | "python3" | "ruby" | "perl" | "node" | "php"
    ) {
        return interpreter_script_arg(&words[(command_index + 1)..]);
    }

    None
}

fn shell_script_arg(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    let mut end_options = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if end_options {
            return Some(arg);
        }
        if arg == "--" {
            end_options = true;
            continue;
        }
        if matches!(arg.as_str(), "-c" | "--command") {
            return None;
        }
        if matches!(arg.as_str(), "--rcfile" | "--init-file") {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

fn interpreter_script_arg(args: &[String]) -> Option<&str> {
    let mut skip_next = false;
    let mut end_options = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if end_options {
            return Some(arg);
        }
        if arg == "--" {
            end_options = true;
            continue;
        }
        if matches!(arg.as_str(), "-c" | "-e" | "--command" | "--eval" | "-m") {
            return None;
        }
        if matches!(arg.as_str(), "-r" | "--require" | "-I" | "-S") {
            skip_next = true;
            continue;
        }
        if arg.starts_with('-') {
            continue;
        }
        return Some(arg);
    }
    None
}

fn shell_words_prefix(command_line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut chars = command_line.chars().peekable();
    let mut quote = None;
    let mut in_word = false;

    while let Some(ch) = chars.next() {
        match quote {
            Some('\'') => {
                if ch == '\'' {
                    quote = None;
                } else {
                    word.push(ch);
                }
            }
            Some('"') => match ch {
                '"' => quote = None,
                '\\' => {
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                _ => word.push(ch),
            },
            Some(_) => unreachable!("only shell quote characters are stored"),
            None => match ch {
                '\'' | '"' => {
                    quote = Some(ch);
                    in_word = true;
                }
                '\\' => {
                    in_word = true;
                    if let Some(next) = chars.next() {
                        word.push(next);
                    }
                }
                ch if ch.is_whitespace() => {
                    if in_word {
                        words.push(std::mem::take(&mut word));
                        in_word = false;
                    }
                }
                '#' if !in_word => break,
                ';' | '|' | '&' | '<' | '>' | '(' | ')' | '`' | '\n' => break,
                _ => {
                    in_word = true;
                    word.push(ch);
                }
            },
        }
    }

    if quote.is_some() {
        return None;
    }
    if in_word {
        words.push(word);
    }
    (!words.is_empty()).then_some(words)
}

fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_path_like(word: &str) -> bool {
    Path::new(word).is_absolute() || word.contains('/') || word.contains('\\')
}

fn has_dynamic_path_syntax(path: &str) -> bool {
    path.chars()
        .any(|ch| matches!(ch, '$' | '*' | '?' | '[' | ']' | '{' | '}' | '~'))
}

#[cfg(unix)]
fn permissions_label(metadata: &fs::Metadata) -> String {
    use std::os::unix::fs::PermissionsExt;

    format!("{:o}", metadata.permissions().mode())
}

#[cfg(not(unix))]
fn permissions_label(metadata: &fs::Metadata) -> String {
    format!("readonly={}", metadata.permissions().readonly())
}

#[cfg(test)]
#[path = "command_content_tests.rs"]
mod tests;

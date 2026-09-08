use std::collections::BTreeMap;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::path::Path;
use std::path::PathBuf;

use codex_rollout::ARCHIVED_SESSIONS_SUBDIR;
use codex_rollout::SESSIONS_SUBDIR;
use walkdir::WalkDir;

const MAX_ROLLOUT_RECORD_BYTES: usize = 2 * 1024 * 1024;

/// Reads JSONL records with a hard allocation cap, returning `None` for oversized records.
pub(super) struct BoundedLineReader {
    reader: BufReader<File>,
    buffer: Vec<u8>,
}

impl BoundedLineReader {
    pub(super) fn new(file: File) -> Self {
        Self {
            reader: BufReader::new(file),
            buffer: Vec::new(),
        }
    }

    pub(super) fn next_line(&mut self) -> std::io::Result<Option<Option<&[u8]>>> {
        self.buffer.clear();
        let mut within_limit = true;
        let mut saw_bytes = false;
        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Ok(saw_bytes.then(|| within_limit.then_some(self.buffer.as_slice())));
            }
            saw_bytes = true;
            let end = available
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(available.len(), |index| index + 1);
            if within_limit {
                let remaining = MAX_ROLLOUT_RECORD_BYTES.saturating_sub(self.buffer.len());
                if end <= remaining {
                    self.buffer.extend_from_slice(&available[..end]);
                } else {
                    within_limit = false;
                }
            }
            let reached_newline = available.get(end.saturating_sub(1)) == Some(&b'\n');
            self.reader.consume(end);
            if reached_newline {
                return Ok(Some(within_limit.then_some(self.buffer.as_slice())));
            }
        }
    }
}

pub(super) fn discover_rollouts(codex_home: &Path) -> (Vec<PathBuf>, u64) {
    let mut logical_paths = BTreeMap::<PathBuf, PathBuf>::new();
    let mut discovery_errors = 0;
    for subdir in [SESSIONS_SUBDIR, ARCHIVED_SESSIONS_SUBDIR] {
        for entry in WalkDir::new(codex_home.join(subdir)).follow_links(false) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(err)
                    if err
                        .io_error()
                        .is_some_and(|err| err.kind() == std::io::ErrorKind::NotFound) =>
                {
                    continue;
                }
                Err(_) => {
                    discovery_errors += 1;
                    continue;
                }
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.into_path();
            let name = path.file_name().and_then(|name| name.to_str());
            let logical = match name {
                Some(name) if name.ends_with(".jsonl") => path.clone(),
                Some(name) if name.ends_with(".jsonl.zst") => {
                    path.with_file_name(name.trim_end_matches(".zst"))
                }
                _ => continue,
            };
            let should_replace = logical_paths
                .get(&logical)
                .is_some_and(|current| current.extension().is_some_and(|ext| ext == "zst"))
                && path.extension().is_some_and(|ext| ext == "jsonl");
            if should_replace || !logical_paths.contains_key(&logical) {
                logical_paths.insert(logical, path);
            }
        }
    }
    (logical_paths.into_values().collect(), discovery_errors)
}

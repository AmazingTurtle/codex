//! Verify plugin-list status rendering and output error handling at the writer boundary.

use super::JsonPluginSource;
use super::ListPluginsArgs;
use super::PluginListEntry;
use super::plugin_status;
use super::write_plugin_list;
use pretty_assertions::assert_eq;
use std::io;
use std::io::Write;

#[derive(Clone, Copy)]
enum Failure {
    Write,
    Flush,
}

struct FailingWriter {
    failure: Failure,
    kind: io::ErrorKind,
}

impl Write for FailingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.failure {
            Failure::Write => Err(io::Error::new(self.kind, "output failed")),
            Failure::Flush => Ok(bytes.len()),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.failure {
            Failure::Write => Ok(()),
            Failure::Flush => Err(io::Error::new(self.kind, "output failed")),
        }
    }
}

#[test]
fn plugin_list_treats_broken_pipe_as_success() {
    for json in [false, true] {
        for failure in [Failure::Write, Failure::Flush] {
            let mut writer = FailingWriter {
                failure,
                kind: io::ErrorKind::BrokenPipe,
            };
            let args = ListPluginsArgs {
                marketplace_name: None,
                json,
                available: true,
            };

            write_plugin_list(&mut writer, Vec::new(), args).unwrap();
        }
    }
}

#[test]
fn plugin_list_preserves_other_output_errors() {
    for json in [false, true] {
        for failure in [Failure::Write, Failure::Flush] {
            for kind in [io::ErrorKind::PermissionDenied, io::ErrorKind::WriteZero] {
                let mut writer = FailingWriter { failure, kind };
                let args = ListPluginsArgs {
                    marketplace_name: None,
                    json,
                    available: true,
                };

                let error = write_plugin_list(&mut writer, Vec::new(), args)
                    .unwrap_err()
                    .downcast::<io::Error>()
                    .unwrap();
                assert_eq!(error.kind(), kind);
                assert_eq!(error.to_string(), "output failed");
            }
        }
    }
}

#[test]
fn debloated_plugin_status_and_json_preserve_enabled_state() {
    let entry = PluginListEntry {
        plugin_id: "browser@openai-bundled".to_string(),
        name: "browser".to_string(),
        marketplace_name: "openai-bundled".to_string(),
        version: Some("1.0.0".to_string()),
        display_version: Some("1.0.0".to_string()),
        installed: true,
        enabled: true,
        debloated: true,
        source: JsonPluginSource::Local {
            path: "/plugin".to_string(),
        },
        marketplace_source: None,
        install_policy: "AVAILABLE",
        auth_policy: "ON_USE",
    };

    assert_eq!(plugin_status(&entry), "installed, enabled, debloated");
    let json = serde_json::to_value(entry).expect("plugin entry should serialize");
    assert_eq!(json["enabled"], true);
    assert_eq!(json["debloated"], true);
}

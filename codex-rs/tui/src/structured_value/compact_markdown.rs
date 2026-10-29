//! Port of compactMarkdown's layout rules. Packing measures UTF-16 Markdown source length,
//! as JavaScript does; the existing Markdown renderer subsequently wraps terminal cells.
//! Traversal is iterative, including the relative JSON Pointer fallback, and omits no values.
use serde_json::Map;
use serde_json::Value;

#[path = "literal.rs"]
mod literal;
use literal::entries;
use literal::escape;
use literal::label;
use literal::leaf;
use literal::pack;
use literal::small;
use literal::value;

pub(super) struct Options<'a> {
    pub(super) title: Option<&'a str>,
    pub(super) width: usize,
    pub(super) short: usize,
    pub(super) max_indent_depth: usize,
}

impl Default for Options<'_> {
    fn default() -> Self {
        Self {
            title: Some("Result"),
            width: 100,
            short: 48,
            max_indent_depth: 4,
        }
    }
}

enum Work<'a> {
    Node {
        name: Option<String>,
        value: &'a Value,
        depth: usize,
        indent: usize,
    },
    Object {
        name: Option<String>,
        entries: Vec<(&'a str, &'a Value)>,
        depth: usize,
        indent: usize,
    },
    Flat {
        value: &'a Value,
        path: String,
        indent: usize,
    },
    Section {
        name: String,
        value: &'a Value,
    },
    Record {
        index: usize,
        fields: &'a Map<String, Value>,
    },
}

struct Writer<'a> {
    options: Options<'a>,
    lines: Vec<String>,
    work: Vec<Work<'a>>,
}

impl<'a> Writer<'a> {
    fn line(&mut self, indent: usize, text: String) {
        self.lines.push(format!("{}{text}", "  ".repeat(indent)));
    }
    fn blank(&mut self) {
        if self.lines.last().is_some_and(|line| !line.is_empty()) {
            self.lines.push(String::new());
        }
    }

    fn node(&mut self, name: Option<String>, value: &'a Value, depth: usize, indent: usize) {
        if leaf(value) {
            let prefix = name.map_or_else(String::new, |name| format!("**{name}:** "));
            self.line(indent, format!("- {prefix}{}", self::value(value)));
        } else if depth >= self.options.max_indent_depth {
            let indent = if let Some(name) = name {
                self.line(
                    indent,
                    format!("- **{name}** (relative JSON Pointer paths)"),
                );
                indent + 1
            } else {
                indent
            };
            self.work.push(Work::Flat {
                value,
                path: String::new(),
                indent,
            });
        } else {
            match value {
                Value::Object(fields) => self.object(name, entries(fields), depth, indent),
                Value::Array(values) => {
                    let indent = if let Some(name) = name {
                        self.line(indent, format!("- **{name}**"));
                        indent + 1
                    } else {
                        indent
                    };
                    if values.iter().all(leaf) {
                        for row in pack(values.iter().map(self::value), self.options.width) {
                            self.line(indent, format!("- {row}"));
                        }
                    } else if values
                        .iter()
                        .all(|value| value.as_array().is_some_and(|row| row.iter().all(leaf)))
                    {
                        for (index, row) in values.iter().filter_map(Value::as_array).enumerate() {
                            let row = if row.is_empty() {
                                "`[]`".into()
                            } else {
                                row.iter().map(self::value).collect::<Vec<_>>().join(" · ")
                            };
                            self.line(indent, format!("{}. {row}", index + 1));
                        }
                    } else {
                        for (index, value) in values.iter().enumerate().rev() {
                            self.work.push(Work::Node {
                                name: Some((index + 1).to_string()),
                                value,
                                depth: depth + 1,
                                indent,
                            });
                        }
                    }
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                    unreachable!("nonleaf collections only")
                }
            }
        }
    }

    fn object(
        &mut self,
        name: Option<String>,
        entries: Vec<(&'a str, &'a Value)>,
        depth: usize,
        indent: usize,
    ) {
        if depth >= self.options.max_indent_depth {
            let indent = if let Some(name) = name {
                self.line(
                    indent,
                    format!("- **{name}** (relative JSON Pointer paths)"),
                );
                indent + 1
            } else {
                indent
            };
            for (key, value) in entries.into_iter().rev() {
                let key = key.replace('~', "~0").replace('/', "~1");
                self.work.push(Work::Flat {
                    value,
                    path: format!("/{key}"),
                    indent,
                });
            }
            return;
        }
        let summary = pack(
            entries
                .iter()
                .filter(|(_, value)| small(value, self.options.short))
                .map(|(key, value)| format!("**{}:** {}", label(key), self::value(value))),
            self.options.width,
        );
        let indent = if let Some(name) = name {
            let first = summary
                .first()
                .map_or_else(String::new, |first| format!(" · {first}"));
            self.line(indent, format!("- **{name}**{first}"));
            for row in summary.into_iter().skip(1) {
                self.line(indent + 1, format!("- {row}"));
            }
            indent + 1
        } else {
            for row in summary {
                self.line(indent, format!("- {row}"));
            }
            indent
        };
        for (key, value) in entries
            .into_iter()
            .rev()
            .filter(|(_, value)| !small(value, self.options.short))
        {
            self.work.push(Work::Node {
                name: Some(label(key)),
                value,
                depth: depth + 1,
                indent,
            });
        }
    }

    fn flat(&mut self, value: &'a Value, path: String, indent: usize) {
        if leaf(value) {
            self.line(
                indent,
                format!("- **{}:** {}", escape(&path), self::value(value)),
            );
        } else {
            let entries = match value {
                Value::Object(fields) => entries(fields),
                Value::Array(values) => {
                    for (index, value) in values.iter().enumerate().rev() {
                        self.work.push(Work::Flat {
                            value,
                            path: format!("{path}/{index}"),
                            indent,
                        });
                    }
                    return;
                }
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                    unreachable!("nonleaf collections only")
                }
            };
            for (key, value) in entries.into_iter().rev() {
                let key = key.replace('~', "~0").replace('/', "~1");
                self.work.push(Work::Flat {
                    value,
                    path: format!("{path}/{key}"),
                    indent,
                });
            }
        }
    }

    fn section(&mut self, name: String, value: &'a Value) {
        self.blank();
        self.line(0, format!("## {name}"));
        self.blank();
        if let Value::Array(values) = value
            && !values.is_empty()
            && values.iter().all(Value::is_object)
        {
            for (index, value) in values.iter().enumerate().rev() {
                if let Value::Object(fields) = value {
                    self.work.push(Work::Record {
                        index: index + 1,
                        fields,
                    });
                }
            }
        } else {
            self.work.push(Work::Node {
                name: None,
                value,
                depth: 0,
                indent: 0,
            });
        }
    }

    fn record(&mut self, index: usize, fields: &'a Map<String, Value>) {
        let entries = entries(fields);
        let identity = entries.iter().find(|(_, value)| {
            value.as_str().is_some_and(|text| !text.is_empty()) && small(value, self.options.short)
        });
        let name = identity.map_or_else(
            || index.to_string(),
            |(key, value)| format!("{index} · {}: {}", label(key), self::value(value)),
        );
        let rest = entries
            .iter()
            .copied()
            .filter(|(key, _)| identity.is_none_or(|(identity, _)| key != identity))
            .collect::<Vec<_>>();
        self.blank();
        self.line(0, format!("### {name}"));
        if !rest.is_empty() {
            self.blank();
            self.work.push(Work::Object {
                name: None,
                entries: rest,
                depth: 0,
                indent: 0,
            });
        } else if identity.is_none() {
            self.blank();
            self.line(0, "- `{}`".into());
        }
    }
}

pub(super) fn format<'a>(data: &'a Value, options: Options<'a>) -> String {
    let mut writer = Writer {
        options,
        lines: Vec::new(),
        work: Vec::new(),
    };
    if let Some(title) = writer.options.title {
        writer.line(0, format!("# {}", escape(title)));
        writer.blank();
    }
    if let Value::Object(fields) = data
        && !fields.is_empty()
    {
        let entries = entries(fields);
        for (key, value) in entries
            .iter()
            .copied()
            .rev()
            .filter(|(_, value)| !leaf(value))
        {
            writer.work.push(Work::Section {
                name: label(key),
                value,
            });
        }
        let metadata = entries
            .into_iter()
            .filter(|(_, value)| leaf(value))
            .collect::<Vec<_>>();
        if !metadata.is_empty() {
            writer.work.push(Work::Object {
                name: None,
                entries: metadata,
                depth: 0,
                indent: 0,
            });
        }
    } else {
        writer.work.push(Work::Node {
            name: None,
            value: data,
            depth: 0,
            indent: 0,
        });
    }
    while let Some(work) = writer.work.pop() {
        match work {
            Work::Node {
                name,
                value,
                depth,
                indent,
            } => writer.node(name, value, depth, indent),
            Work::Object {
                name,
                entries,
                depth,
                indent,
            } => writer.object(name, entries, depth, indent),
            Work::Flat {
                value,
                path,
                indent,
            } => writer.flat(value, path, indent),
            Work::Section { name, value } => writer.section(name, value),
            Work::Record { index, fields } => writer.record(index, fields),
        }
    }
    writer.lines.join("\n") + "\n"
}

#[cfg(test)]
#[path = "compact_markdown_tests.rs"]
mod tests;

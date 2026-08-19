#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StatusAccountDisplay {
    ChatGpt { name: Option<String> },
    ApiKey,
}

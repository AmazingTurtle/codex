//! Stable provider-sized receipt identities, including legacy request-only aliases.

use codex_protocol::ResponseItemId;
use uuid::Uuid;

pub(super) fn terminal_notice_ids(legacy_id: &ResponseItemId) -> [ResponseItemId; 3] {
    let identity = Uuid::new_v5(&Uuid::NAMESPACE_OID, legacy_id.as_bytes());
    [
        ResponseItemId::with_suffix("amsg", identity),
        ResponseItemId::with_suffix("workflow", identity),
        legacy_id.clone(),
    ]
}

/// Old notices remain durable receipt keys. Only their outbound request copy changes.
pub(crate) fn legacy_notice_id_for_request(id: &ResponseItemId) -> Option<ResponseItemId> {
    let suffix = id.strip_prefix("workflow_")?;
    if id.len() == 45 {
        let identity = Uuid::parse_str(suffix).ok()?;
        if identity.to_string() == suffix && identity.get_version() == Some(uuid::Version::Sha1) {
            return Some(ResponseItemId::with_suffix("amsg", identity));
        }
        return None;
    }
    if id.len() != 82 {
        return None;
    }
    let (run, revision) = suffix.split_at_checked(36)?;
    let revision = revision.strip_prefix('-')?;
    for value in [run, revision] {
        if Uuid::parse_str(value).ok()?.to_string() != value {
            return None;
        }
    }
    let [canonical, _, _] = terminal_notice_ids(id);
    Some(canonical)
}

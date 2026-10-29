//! Local admission disposition is reconciled only after its native scheduling barrier.

use super::ACCEPTED;
use super::ADMITTED;
use super::PREPARED;
use super::REJECTED;
use super::StartReceipt;
use std::sync::atomic::Ordering;
use tokio_util::sync::CancellationToken;

pub(in crate::workflows) enum StartDisposition {
    Prepared,
    Uncertain,
    Accepted,
    Rejected,
}

impl StartReceipt {
    pub(in crate::workflows) fn attempt_id(&self) -> &str {
        &self.attempt
    }

    pub(in crate::workflows) fn revision_id(&self) -> &str {
        &self.revision_id
    }

    pub(in crate::workflows) fn admission_cancellation(&self) -> CancellationToken {
        self.admission_cancellation.clone()
    }

    pub(in crate::workflows) fn cancel_prepared(&self) {
        if self.state.load(Ordering::Acquire) == PREPARED {
            self.admission_cancellation.cancel();
        }
    }

    pub(in crate::workflows) fn disposition(&self) -> StartDisposition {
        match self.state.load(Ordering::Acquire) {
            PREPARED => StartDisposition::Prepared,
            ADMITTED => StartDisposition::Uncertain,
            ACCEPTED => StartDisposition::Accepted,
            REJECTED => StartDisposition::Rejected,
            _ => unreachable!("native workflow receipt has an invalid state"),
        }
    }
}

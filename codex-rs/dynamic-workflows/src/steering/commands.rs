//! Commands retain their original target and arguments across retries and restarts.

use super::WorkflowControlCommand;
use super::WorkflowControlRequest;
use super::WorkflowControlResponse;
use super::WorkflowControlStatus;
use super::WorkflowSteeringState;
use crate::WorkflowRevisionStatus;

impl WorkflowSteeringState {
    pub(crate) fn enqueue_control(
        &mut self,
        request: &WorkflowControlRequest,
    ) -> Result<WorkflowControlResponse, String> {
        request.validate()?;
        let payload = serde_json::to_value(request).map_err(|error| error.to_string())?;
        if self
            .repeated_mutation(&request.command_id, &payload)?
            .is_some()
        {
            return self.control_response(&request.command_id);
        }
        self.check_revision(request.expected_control_revision)?;
        if self.state.status != WorkflowRevisionStatus::Active {
            return Err(
                "workflow revision is fenced; resume the ready revision before changing a step"
                    .into(),
            );
        }
        if self.state.active_revision_id.is_empty() {
            return Err("workflow plan identity is not initialized".into());
        }
        self.remember_mutation(&request.command_id, payload, None)?;
        let command = WorkflowControlCommand {
            id: request.command_id.clone(),
            revision_id: self.state.active_revision_id.clone(),
            control_revision: self.state.control_revision,
            created_at: crate::types::timestamp(),
            target: request.target.clone(),
            operation: request.operation.clone(),
            status: WorkflowControlStatus::Pending,
            error: None,
        };
        self.commands.push(command);
        self.control_response(&request.command_id)
    }

    pub(crate) fn control_response(&self, id: &str) -> Result<WorkflowControlResponse, String> {
        Ok(WorkflowControlResponse {
            command: self
                .commands
                .iter()
                .find(|command| command.id == id)
                .cloned()
                .ok_or("workflow control command not found")?,
            state: self.state.clone(),
        })
    }

    pub(crate) fn finish_control(
        &mut self,
        id: &str,
        status: WorkflowControlStatus,
        error: Option<String>,
    ) -> Result<WorkflowControlResponse, String> {
        let command = self
            .commands
            .iter_mut()
            .find(|command| command.id == id)
            .ok_or("workflow control command not found")?;
        if command.status != WorkflowControlStatus::Pending {
            if command.status == status && command.error == error {
                return self.control_response(id);
            }
            return Err("workflow control command already has a different disposition".into());
        }
        command.status = status;
        command.error = error.map(|mut error| {
            if error.len() > 2048 {
                let mut end = 2048;
                while !error.is_char_boundary(end) {
                    end -= 1;
                }
                error.truncate(end);
            }
            error
        });
        self.control_response(id)
    }
}

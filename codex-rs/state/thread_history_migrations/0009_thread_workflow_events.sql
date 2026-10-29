CREATE TABLE thread_workflow_events (
    thread_id TEXT NOT NULL,
    event_id TEXT NOT NULL,
    rollout_ordinal INTEGER NOT NULL,
    event_json TEXT NOT NULL,
    PRIMARY KEY (thread_id, event_id)
);
CREATE INDEX thread_workflow_events_timeline
    ON thread_workflow_events(thread_id, rollout_ordinal, event_id);

CREATE TRIGGER thread_workflow_events_projection_cleanup
    AFTER DELETE ON thread_history_projection_state
BEGIN
    DELETE FROM thread_workflow_events WHERE thread_id = OLD.thread_id;
END;

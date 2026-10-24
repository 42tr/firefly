CREATE TABLE assistant_memory_jobs (
    generation_job_id BLOB PRIMARY KEY REFERENCES assistant_generation_jobs(id) ON DELETE CASCADE,
    conversation_id INTEGER NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    final_message_id INTEGER NOT NULL REFERENCES assistant_messages(id) ON DELETE CASCADE,
    status TEXT NOT NULL DEFAULT 'queued' CHECK (status IN ('queued', 'running', 'completed', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    error_message TEXT,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    started_at INTEGER,
    completed_at INTEGER
);

CREATE INDEX idx_assistant_memory_jobs_queue
    ON assistant_memory_jobs(status, created_at);

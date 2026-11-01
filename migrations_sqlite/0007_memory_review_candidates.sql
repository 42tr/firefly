ALTER TABLE assistant_memory_jobs ADD COLUMN memory_snapshot TEXT;

CREATE TABLE assistant_memory_review_candidates (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    generation_job_id BLOB NOT NULL REFERENCES assistant_memory_jobs(generation_job_id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    memory_id INTEGER,
    title TEXT NOT NULL CHECK (length(title) <= 400),
    content TEXT NOT NULL,
    evidence TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'completed', 'dismissed')),
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

CREATE INDEX idx_assistant_memory_review_candidates_pending
    ON assistant_memory_review_candidates(user_id, status, created_at, id);

CREATE INDEX idx_assistant_memory_review_candidates_job
    ON assistant_memory_review_candidates(generation_job_id, status);

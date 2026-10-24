CREATE TABLE assistant_context_summaries (
    conversation_id INTEGER PRIMARY KEY REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    upto_message_id INTEGER NOT NULL REFERENCES assistant_messages(id) ON DELETE CASCADE,
    content TEXT NOT NULL CHECK (length(CAST(content AS BLOB)) <= 32768),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

CREATE INDEX idx_assistant_context_summaries_user
    ON assistant_context_summaries(user_id, updated_at);

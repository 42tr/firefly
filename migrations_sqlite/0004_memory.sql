CREATE TABLE assistant_memories (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    title TEXT NOT NULL CHECK (length(title) <= 400),
    content TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT TRUE CHECK (enabled IN (0, 1)),
    source TEXT NOT NULL DEFAULT 'manual' CHECK (source IN ('manual', 'agent')),
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

CREATE INDEX idx_assistant_memories_user ON assistant_memories(user_id, updated_at DESC);

CREATE TABLE assistant_memory_revisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    memory_id INTEGER NOT NULL REFERENCES assistant_memories(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    previous_updated_at INTEGER NOT NULL,
    evidence TEXT NOT NULL,
    replaced_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

CREATE INDEX idx_assistant_memory_revisions_memory ON assistant_memory_revisions(memory_id, id);

CREATE TABLE assistant_memory_embeddings (
    memory_id INTEGER PRIMARY KEY REFERENCES assistant_memories(id) ON DELETE CASCADE,
    embedding_space TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    dimensions INTEGER NOT NULL CHECK (dimensions > 0),
    chunks TEXT NOT NULL CHECK (json_valid(chunks)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

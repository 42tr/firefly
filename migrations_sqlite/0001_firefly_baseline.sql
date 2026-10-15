-- Fresh-install baseline. Previous development databases are not supported.
-- Initialize a new database with firefly --init-db; keep old databases as backups.

CREATE TABLE assistant_conversations (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    title TEXT CHECK (length(title) <= 255),
    system_prompt TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (created_at IS NULL OR typeof(created_at) = 'integer'),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (updated_at IS NULL OR typeof(updated_at) = 'integer'),
    last_message_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (last_message_at IS NULL OR typeof(last_message_at) = 'integer'),
    message_count INTEGER NOT NULL DEFAULT 0,
    agent TEXT NOT NULL DEFAULT 'firefly' CHECK (length(agent) <= 16),
    codex_cwd TEXT,
    codex_thread_id TEXT,
    claude_session_id TEXT,
    codex_provider TEXT,
    codex_model TEXT,
    workspace_mode TEXT NOT NULL DEFAULT 'shared',
    source_cwd TEXT,
    workspace_id BLOB CHECK (workspace_id IS NULL OR (typeof(workspace_id) = 'blob' AND length(workspace_id) = 16)),
    workspace_state TEXT NOT NULL DEFAULT 'ready',
    workspace_error TEXT,
    workspace_metadata TEXT CHECK (workspace_metadata IS NULL OR json_valid(workspace_metadata)),
    codex_reasoning_level TEXT,
    is_pinned BOOLEAN NOT NULL DEFAULT FALSE CHECK (is_pinned IN (FALSE, TRUE)),
    read_message_id INTEGER NOT NULL DEFAULT 0 CHECK (read_message_id >= 0),
    is_archived BOOLEAN NOT NULL DEFAULT FALSE CHECK (is_archived IN (FALSE, TRUE)),
    claude_model TEXT,
    CHECK (agent IN ('firefly', 'codex', 'claude')),
    CHECK (workspace_mode IN ('shared', 'parallel')),
    CHECK (workspace_state IN ('pending', 'preparing', 'ready', 'failed', 'cancelled', 'missing')),
    CHECK (workspace_mode = 'shared' OR (agent IN ('codex', 'claude')
        AND source_cwd IS NOT NULL AND workspace_id IS NOT NULL AND codex_cwd IS NOT NULL))
);

CREATE UNIQUE INDEX assistant_conversations_workspace_id ON assistant_conversations (workspace_id) WHERE (workspace_id IS NOT NULL);

CREATE UNIQUE INDEX idx_assistant_conversations_claude_session ON assistant_conversations (claude_session_id) WHERE (claude_session_id IS NOT NULL);

CREATE UNIQUE INDEX idx_assistant_conversations_codex_thread ON assistant_conversations (codex_thread_id) WHERE (codex_thread_id IS NOT NULL);

CREATE INDEX idx_assistant_conversations_updated_at
    ON assistant_conversations (updated_at, id);

CREATE INDEX idx_assistant_conversations_user
    ON assistant_conversations (user_id, is_archived, is_pinned DESC, last_message_at DESC, id DESC);

CREATE INDEX idx_assistant_workspace_pages
    ON assistant_conversations (user_id, is_archived, is_pinned DESC, last_message_at DESC, id DESC)
    WHERE message_count > 0;

CREATE TABLE assistant_generation_jobs (
    id BLOB NOT NULL CHECK (id IS NULL OR (typeof(id) = 'blob' AND length(id) = 16)),
    conversation_id INTEGER NOT NULL,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    status TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('queued','running','completed','failed')),
    error_message TEXT,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (created_at IS NULL OR typeof(created_at) = 'integer'),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (updated_at IS NULL OR typeof(updated_at) = 'integer'),
    enable_tools INTEGER NOT NULL DEFAULT true CHECK (enable_tools IN (0, 1)),
    started_at INTEGER,
    completed_at INTEGER,
    codex_settings TEXT CHECK (codex_settings IS NULL OR json_valid(codex_settings)),
    claude_settings TEXT CHECK (claude_settings IS NULL OR json_valid(claude_settings)),
    PRIMARY KEY (id),
    FOREIGN KEY (conversation_id) REFERENCES assistant_conversations(id) ON DELETE CASCADE
);

CREATE INDEX idx_assistant_generation_jobs_conversation ON assistant_generation_jobs (conversation_id, created_at DESC);

CREATE INDEX idx_assistant_generation_jobs_queue ON assistant_generation_jobs (conversation_id, created_at, id) WHERE (status = 'queued');

CREATE UNIQUE INDEX idx_assistant_generation_jobs_running_conversation ON assistant_generation_jobs (conversation_id) WHERE (status = 'running');

CREATE INDEX idx_assistant_generation_jobs_active
    ON assistant_generation_jobs(conversation_id)
    WHERE status IN ('queued', 'running');

CREATE TABLE assistant_messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL,
    role TEXT NOT NULL CHECK (length(role) <= 20),
    content TEXT NOT NULL,
    tool_call_id TEXT CHECK (length(tool_call_id) <= 255),
    tool_calls TEXT CHECK (tool_calls IS NULL OR json_valid(tool_calls)),
    metadata TEXT CHECK (metadata IS NULL OR json_valid(metadata)),
    is_error INTEGER NOT NULL DEFAULT false CHECK (is_error IN (0, 1)),
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (created_at IS NULL OR typeof(created_at) = 'integer'),
    generation_job_id BLOB CHECK (generation_job_id IS NULL OR (typeof(generation_job_id) = 'blob' AND length(generation_job_id) = 16)),
    FOREIGN KEY (conversation_id) REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (generation_job_id) REFERENCES assistant_generation_jobs(id) ON DELETE SET NULL
);

CREATE INDEX idx_assistant_messages_conversation ON assistant_messages (conversation_id, created_at);

CREATE INDEX idx_assistant_messages_generation_job ON assistant_messages (generation_job_id) WHERE (generation_job_id IS NOT NULL);

CREATE UNIQUE INDEX idx_assistant_messages_generation_user ON assistant_messages (generation_job_id) WHERE generation_job_id IS NOT NULL AND role = 'user';

CREATE INDEX idx_assistant_messages_tool_call ON assistant_messages (tool_call_id);

CREATE INDEX idx_assistant_messages_user_turns ON assistant_messages (id DESC) WHERE role = 'user';

CREATE INDEX idx_assistant_messages_visible_bubble ON assistant_messages (conversation_id, id DESC) WHERE role = 'user' OR (role = 'assistant' AND trim(content) <> '');

CREATE INDEX idx_assistant_messages_conversation_id
    ON assistant_messages(conversation_id, id DESC, generation_job_id);

CREATE INDEX idx_assistant_messages_legacy_user
    ON assistant_messages(conversation_id, id DESC)
    WHERE role = 'user' AND generation_job_id IS NULL;

CREATE INDEX idx_assistant_messages_remote_execution
    ON assistant_messages(conversation_id, generation_job_id)
    WHERE json_extract(metadata, '$.remote_execution') IS NOT NULL;

CREATE TABLE assistant_message_attachments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    message_id INTEGER NOT NULL,
    file_name TEXT NOT NULL CHECK (length(file_name) <= 255),
    content_type TEXT NOT NULL CHECK (length(content_type) <= 255),
    file_size INTEGER NOT NULL,
    storage_path TEXT NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)) CHECK (created_at IS NULL OR typeof(created_at) = 'integer'),
    FOREIGN KEY (message_id) REFERENCES assistant_messages(id) ON DELETE CASCADE
);

CREATE INDEX idx_assistant_message_attachments_message ON assistant_message_attachments (message_id);

CREATE TABLE assistant_firefly_branches (
    conversation_id INTEGER NOT NULL,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    request_id BLOB NOT NULL CHECK (request_id IS NULL OR (typeof(request_id) = 'blob' AND length(request_id) = 16)),
    reply_to_message_id INTEGER,
    reply_preview TEXT,
    context TEXT NOT NULL DEFAULT '[]' CHECK (context IS NULL OR json_valid(context)),
    PRIMARY KEY (conversation_id),
    UNIQUE (user_id, request_id),
    FOREIGN KEY (conversation_id) REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    FOREIGN KEY (reply_to_message_id) REFERENCES assistant_messages(id) ON DELETE SET NULL
);

CREATE TABLE assistant_workspace_submissions (
    user_id TEXT NOT NULL,
    conversation_id INTEGER NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    request_id BLOB NOT NULL CHECK (typeof(request_id) = 'blob' AND length(request_id) = 16),
    payload_sha256 TEXT NOT NULL CHECK (length(payload_sha256) = 64),
    generation_job_id BLOB REFERENCES assistant_generation_jobs(id) ON DELETE SET NULL,
    PRIMARY KEY (user_id, conversation_id, request_id)
);

CREATE INDEX idx_workspace_submissions_job ON assistant_workspace_submissions(generation_job_id);

CREATE TABLE assistant_tasks (
    id TEXT PRIMARY KEY,
    user_id TEXT NOT NULL,
    conversation_id INTEGER NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    generation_job_id TEXT NOT NULL,
    run_id TEXT NOT NULL,
    source_call_id TEXT NOT NULL,
    source_index INTEGER NOT NULL,
    parent_task_id TEXT REFERENCES assistant_tasks(id),
    title TEXT NOT NULL,
    tool TEXT NOT NULL,
    arguments TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'queued',
    detached INTEGER NOT NULL DEFAULT 0,
    background_allowed INTEGER NOT NULL DEFAULT 0,
    observed INTEGER NOT NULL DEFAULT 0,
    parallel_safe INTEGER NOT NULL DEFAULT 0,
    resource_key TEXT,
    execution_timeout_secs INTEGER NOT NULL,
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    deadline_at INTEGER,
    ended_at INTEGER,
    result TEXT,
    error TEXT,
    scope_id TEXT,
    UNIQUE(generation_job_id, run_id, source_call_id, source_index)
);

CREATE INDEX assistant_tasks_queue ON assistant_tasks(status, created_at);

CREATE INDEX assistant_tasks_job ON assistant_tasks(generation_job_id);

CREATE INDEX assistant_tasks_conversation ON assistant_tasks(user_id, conversation_id, created_at);

CREATE INDEX assistant_tasks_scope ON assistant_tasks(generation_job_id, scope_id);

CREATE TABLE assistant_task_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES assistant_tasks(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    data TEXT NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE INDEX assistant_task_events_task ON assistant_task_events(task_id, seq);

CREATE INDEX assistant_task_events_summary
    ON assistant_task_events(task_id, seq, kind, created_at)
    WHERE kind != 'output';

CREATE TABLE assistant_tool_details (
    conversation_id INTEGER NOT NULL REFERENCES assistant_conversations(id) ON DELETE CASCADE,
    job_id TEXT NOT NULL,
    call_id TEXT NOT NULL,
    parent_id TEXT NOT NULL DEFAULT '',
    name TEXT NOT NULL,
    input JSON,
    output JSON,
    status TEXT NOT NULL DEFAULT 'running',
    created_at INTEGER,
    started_at INTEGER,
    ended_at INTEGER,
    revision INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (conversation_id, job_id, call_id, parent_id)
);

CREATE INDEX assistant_tool_details_revision
    ON assistant_tool_details(conversation_id, revision);

CREATE TABLE assistant_schedules (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id TEXT NOT NULL,
    title TEXT NOT NULL,
    prompt TEXT NOT NULL,
    cron TEXT NOT NULL,
    execution TEXT NOT NULL CHECK (json_valid(execution)),
    enabled INTEGER NOT NULL DEFAULT TRUE CHECK (enabled IN (0, 1)),
    next_run_at INTEGER,
    last_run_at INTEGER,
    last_conversation_id INTEGER REFERENCES assistant_conversations(id) ON DELETE SET NULL,
    last_job_id BLOB REFERENCES assistant_generation_jobs(id) ON DELETE SET NULL,
    last_notice TEXT,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    timezone TEXT NOT NULL DEFAULT 'Asia/Shanghai',
    CHECK ((enabled = 0 AND next_run_at IS NULL) OR (enabled = 1 AND next_run_at IS NOT NULL))
);

CREATE INDEX idx_assistant_schedules_due ON assistant_schedules(next_run_at) WHERE enabled = TRUE;

CREATE INDEX idx_assistant_schedules_user ON assistant_schedules(user_id, id);

CREATE TABLE model_limits_cache (
    cache_key BLOB PRIMARY KEY NOT NULL CHECK (length(cache_key) = 32),
    limits TEXT NOT NULL CHECK (json_valid(limits)),
    discovered_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER))
);

CREATE TABLE external_module_settings (
    id TEXT PRIMARY KEY,
    enabled INTEGER NOT NULL DEFAULT 0 CHECK(enabled IN (0, 1)),
    settings TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(settings)),
    enabled_jobs TEXT NOT NULL DEFAULT '[]' CHECK(json_valid(enabled_jobs))
);

CREATE TABLE external_module_jobs (
    module_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    definition TEXT NOT NULL,
    next_run_at INTEGER NOT NULL,
    PRIMARY KEY(module_id, job_id)
);

CREATE TABLE external_module_runs (
    id TEXT PRIMARY KEY,
    module_id TEXT NOT NULL,
    job_id TEXT NOT NULL,
    scheduled_at INTEGER NOT NULL,
    status TEXT NOT NULL CHECK(status IN ('running', 'completed', 'failed', 'unknown')),
    result TEXT,
    error TEXT,
    ended_at INTEGER,
    UNIQUE(module_id, job_id, scheduled_at)
);

CREATE INDEX external_module_runs_recent ON external_module_runs(module_id, scheduled_at DESC);

CREATE TABLE assistant_agent_settings (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    revision INTEGER NOT NULL DEFAULT 1,
    settings TEXT NOT NULL CHECK (json_valid(settings)),
    updated_at INTEGER NOT NULL
);

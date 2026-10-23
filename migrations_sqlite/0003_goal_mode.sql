CREATE TABLE assistant_goals (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    conversation_id INTEGER NOT NULL,
    user_id TEXT NOT NULL CHECK (length(user_id) <= 255),
    prompt TEXT NOT NULL CHECK (length(prompt) <= 100000),
    status TEXT NOT NULL DEFAULT 'paused' CHECK (status IN ('planning','running','checking','continuing','paused','completed','needs_review','limit_reached','failed','cancelled')),
    auto_continue INTEGER NOT NULL DEFAULT TRUE CHECK (auto_continue IN (0, 1)),
    current_stage_id TEXT,
    turn_count INTEGER NOT NULL DEFAULT 0 CHECK (turn_count >= 0),
    max_turns INTEGER NOT NULL DEFAULT 50 CHECK (max_turns BETWEEN 1 AND 100),
    max_runtime_secs INTEGER NOT NULL DEFAULT 86400 CHECK (max_runtime_secs BETWEEN 60 AND 604800),
    last_check_result TEXT CHECK (last_check_result IS NULL OR json_valid(last_check_result)),
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    updated_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    paused_at INTEGER,
    completed_at INTEGER,
    FOREIGN KEY (conversation_id) REFERENCES assistant_conversations(id) ON DELETE CASCADE
);

CREATE INDEX idx_assistant_goals_conversation ON assistant_goals(conversation_id, created_at DESC);
CREATE INDEX idx_assistant_goals_user ON assistant_goals(user_id, updated_at DESC);
CREATE UNIQUE INDEX idx_assistant_goals_active_conversation
    ON assistant_goals(conversation_id)
    WHERE status IN ('planning','running','checking','continuing');

CREATE TABLE assistant_goal_stages (
    id TEXT PRIMARY KEY,
    goal_id INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    title TEXT NOT NULL CHECK (length(title) <= 255),
    summary TEXT NOT NULL DEFAULT '' CHECK (length(summary) <= 2000),
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending','active','completed','blocked')),
    started_at INTEGER,
    completed_at INTEGER,
    FOREIGN KEY (goal_id) REFERENCES assistant_goals(id) ON DELETE CASCADE,
    UNIQUE(goal_id, ordinal)
);

CREATE INDEX idx_assistant_goal_stages_goal ON assistant_goal_stages(goal_id, ordinal);

CREATE TABLE assistant_goal_turns (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    goal_id INTEGER NOT NULL,
    generation_job_id BLOB,
    turn_index INTEGER NOT NULL CHECK (turn_index > 0),
    origin TEXT NOT NULL CHECK (origin IN ('goal_start','manual','goal_continue')),
    check_status TEXT NOT NULL DEFAULT 'pending' CHECK (check_status IN ('pending','checking','completed','continue','needs_review','failed','skipped')),
    judge_summary TEXT,
    remaining_work TEXT CHECK (remaining_work IS NULL OR json_valid(remaining_work)),
    continuation_prompt TEXT,
    created_at INTEGER NOT NULL DEFAULT (CAST(unixepoch('subsec') * 1000000 AS INTEGER)),
    checked_at INTEGER,
    FOREIGN KEY (goal_id) REFERENCES assistant_goals(id) ON DELETE CASCADE,
    FOREIGN KEY (generation_job_id) REFERENCES assistant_generation_jobs(id) ON DELETE SET NULL,
    UNIQUE(goal_id, generation_job_id),
    UNIQUE(goal_id, turn_index)
);

CREATE INDEX idx_assistant_goal_turns_goal ON assistant_goal_turns(goal_id, turn_index DESC);

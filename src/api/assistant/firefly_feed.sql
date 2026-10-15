WITH turns AS (
    SELECT u.*, j.status AS status, j.error_message,
           COALESCE(b.reply_to_message_id, previous.id) AS reply_id,
           COALESCE(b.reply_preview, substr(previous.content, 1, 160)) AS reply_preview
    FROM assistant_messages u
    JOIN assistant_conversations c ON c.id = u.conversation_id
    LEFT JOIN assistant_generation_jobs j ON j.id = u.generation_job_id
    LEFT JOIN assistant_firefly_branches b ON b.conversation_id = c.id
    LEFT JOIN assistant_messages previous ON previous.id = (
        SELECT p.id FROM assistant_messages previous_user
        JOIN assistant_messages p ON p.conversation_id = previous_user.conversation_id
          AND (p.generation_job_id = previous_user.generation_job_id OR
               (previous_user.generation_job_id IS NULL AND p.generation_job_id IS NULL
                AND p.id > previous_user.id AND p.id < u.id))
        WHERE b.conversation_id IS NULL AND previous_user.conversation_id = u.conversation_id
          AND previous_user.role = 'user' AND previous_user.id < u.id
          AND p.role = 'assistant' AND trim(p.content) <> ''
          AND (p.tool_calls IS NULL OR p.tool_calls = '[]')
        ORDER BY previous_user.id DESC, p.id DESC LIMIT 1
    )
    WHERE c.user_id = ?1 AND c.agent = 'firefly' AND u.role = 'user'
      AND (?2 IS NULL OR u.id < ?2)
      AND (?3 IS NULL OR u.id = ?3)
      AND (?4 IS NULL OR EXISTS (
          SELECT 1 FROM assistant_messages target WHERE target.id = ?4
          AND target.conversation_id = u.conversation_id AND (
              target.generation_job_id = u.generation_job_id OR
              (target.generation_job_id IS NULL AND u.generation_job_id IS NULL AND target.id >= u.id
               AND NOT EXISTS (SELECT 1 FROM assistant_messages next_u WHERE next_u.conversation_id = u.conversation_id
                               AND next_u.role = 'user' AND next_u.id > u.id AND next_u.id <= target.id))
          )
      ))
    ORDER BY u.id DESC LIMIT ?5
)
SELECT id, conversation_id, generation_job_id, status, error_message, reply_id, reply_preview
FROM turns ORDER BY id DESC

//! Attachment writes and lookups scoped to the owning conversation.
use sqlx::{query_as, QueryBuilder, Sqlite, SqliteConnection};

use super::{AttachmentRecord, NewAttachment};
use crate::db;

pub async fn fetch_attachments_for_messages(
    conversation_id: i64, user_id: &str, message_ids: &[i64],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if message_ids.is_empty() {
        return Ok(Vec::new());
    }

    let pool = db::pool();
    let mut builder = QueryBuilder::<Sqlite>::new(
        r#"
        SELECT
            a.id,
            a.message_id,
            a.file_name,
            a.content_type,
            a.file_size,
            a.storage_path,
            a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE m.conversation_id = "#,
    );
    builder.push_bind(conversation_id).push(" AND c.user_id = ").push_bind(user_id).push(" AND m.id IN (");
    {
        let mut separated = builder.separated(", ");
        for message_id in message_ids {
            separated.push_bind(message_id);
        }
    }
    builder.push(") ORDER BY a.id ASC");
    let rows = builder.build_query_as::<AttachmentRecord>().fetch_all(pool).await?;
    Ok(rows)
}

pub async fn insert_attachments(
    message_id: i64, attachments: &[NewAttachment],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }

    db::retry_busy("insert assistant attachments", || async {
        let mut tx = db::pool().begin_with("BEGIN IMMEDIATE").await?;
        let records = insert_attachment_rows(&mut tx, message_id, attachments).await?;
        tx.commit().await?;
        Ok(records)
    })
    .await
}

pub(super) async fn insert_attachment_rows(
    conn: &mut SqliteConnection, message_id: i64, attachments: &[NewAttachment],
) -> anyhow::Result<Vec<AttachmentRecord>> {
    if attachments.is_empty() {
        return Ok(Vec::new());
    }

    let mut query_builder = QueryBuilder::<Sqlite>::new(
        "INSERT INTO assistant_message_attachments (message_id, file_name, content_type, file_size, storage_path) ",
    );

    query_builder.push_values(attachments, |mut b, attachment| {
        b.push_bind(message_id)
            .push_bind(&attachment.file_name)
            .push_bind(&attachment.content_type)
            .push_bind(attachment.file_size)
            .push_bind(&attachment.storage_path);
    });

    query_builder.push(" RETURNING id, message_id, file_name, content_type, file_size, storage_path, created_at");

    Ok(query_builder.build_query_as::<AttachmentRecord>().fetch_all(&mut *conn).await?)
}

pub async fn get_attachment_for_user(attachment_id: i64, user_id: &str) -> anyhow::Result<Option<AttachmentRecord>> {
    let pool = db::pool();
    let record = query_as::<_, AttachmentRecord>(
        r#"
        SELECT
            a.id,
            a.message_id,
            a.file_name,
            a.content_type,
            a.file_size,
            a.storage_path,
            a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE a.id = ?1 AND c.user_id = ?2
        "#,
    )
    .bind(attachment_id)
    .bind(user_id)
    .fetch_optional(pool)
    .await?;
    Ok(record)
}

// 上传文件使用 UUID 作为磁盘文件名；始终通过所属会话校验访问权限。
pub async fn get_attachment_by_file_for_user(
    file_id: uuid::Uuid, user_id: &str,
) -> anyhow::Result<Option<AttachmentRecord>> {
    get_attachment_by_file_with_pool(db::pool(), file_id, user_id).await
}

pub(super) async fn get_attachment_by_file_with_pool(
    pool: &sqlx::SqlitePool, file_id: uuid::Uuid, user_id: &str,
) -> anyhow::Result<Option<AttachmentRecord>> {
    let file_name = file_id.to_string();
    Ok(query_as::<_, AttachmentRecord>(
        r#"
        SELECT a.id, a.message_id, a.file_name, a.content_type,
               a.file_size, a.storage_path, a.created_at
        FROM assistant_message_attachments a
        JOIN assistant_messages m ON m.id = a.message_id
        JOIN assistant_conversations c ON c.id = m.conversation_id
        WHERE (a.storage_path = ?1 OR a.storage_path LIKE ?2) AND c.user_id = ?3
        LIMIT 1
        "#,
    )
    .bind(&file_name)
    .bind(format!("%/{file_name}"))
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

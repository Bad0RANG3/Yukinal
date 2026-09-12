//! Durable Agent conversation storage: sessions, messages, search and archive state.

use rusqlite::{params, OptionalExtension, Row};

use super::decode::decode_error;
use crate::models::{ChatMessage, ChatMessageRole, ChatSession, ChatSessionCounts};
use crate::{Database, DatabaseError, Result};

pub struct ChatRepository<'a> {
    db: &'a Database,
}

impl<'a> ChatRepository<'a> {
    pub fn new(db: &'a Database) -> Self {
        Self { db }
    }

    pub fn create(&self, session: &ChatSession) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO chat_sessions (
                    id, workspace_id, server_id, title, created_at, updated_at, archived_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    session.id,
                    session.workspace_id,
                    session.server_id,
                    session.title,
                    session.created_at,
                    session.updated_at,
                    session.archived_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get(&self, id: &str) -> Result<ChatSession> {
        self.db.with(|connection| {
            let sql = session_query("WHERE s.id = ?1");
            connection
                .query_row(&sql, params![id], row_to_session)
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    /// Search title and message content while keeping archive state as an exact filter.
    ///
    /// `offset` pages the same ordering the caller sees (`updated_at DESC, id DESC`). A
    /// page boundary is therefore reproducible only while nothing is written in between;
    /// a conversation that gains a message mid-page moves to the top of the next one
    /// instead of vanishing, which is the failure mode a rows-skipped cursor cannot have
    /// but a stable sort key can.
    pub fn list(
        &self,
        query: Option<&str>,
        archived: Option<bool>,
        limit: usize,
        offset: usize,
    ) -> Result<Vec<ChatSession>> {
        let archive_clause = match archived {
            Some(true) => "AND s.archived_at IS NOT NULL",
            Some(false) => "AND s.archived_at IS NULL",
            None => "",
        };
        let sql = format!(
            r#"{SESSION_SELECT}
             WHERE (s.title LIKE ?1 ESCAPE '\'
                OR EXISTS (
                    SELECT 1 FROM chat_messages search_messages
                     WHERE search_messages.session_id = s.id
                       AND search_messages.content LIKE ?1 ESCAPE '\'
                ))
               {archive_clause}
             GROUP BY s.id
             ORDER BY s.updated_at DESC, s.id DESC
             LIMIT ?2 OFFSET ?3"#,
        );
        let pattern = format!("%{}%", escape_like(query.unwrap_or_default().trim()));
        self.db.with(|connection| {
            let mut statement = connection.prepare(&sql)?;
            let rows = statement.query_map(
                params![pattern, limit as i64, offset as i64],
                row_to_session,
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Count the same search the list runs, split by archive state and ignoring the
    /// list's own archive filter — the filter control needs both numbers at once.
    pub fn counts(&self, query: Option<&str>) -> Result<ChatSessionCounts> {
        let pattern = format!("%{}%", escape_like(query.unwrap_or_default().trim()));
        self.db.with(|connection| {
            connection
                .query_row(
                    r#"SELECT
                     COALESCE(SUM(CASE WHEN s.archived_at IS NULL THEN 1 ELSE 0 END), 0),
                     COALESCE(SUM(CASE WHEN s.archived_at IS NOT NULL THEN 1 ELSE 0 END), 0)
                     FROM chat_sessions s
                    WHERE (s.title LIKE ?1 ESCAPE '\'
                       OR EXISTS (
                           SELECT 1 FROM chat_messages search_messages
                            WHERE search_messages.session_id = s.id
                              AND search_messages.content LIKE ?1 ESCAPE '\'
                       ))"#,
                    params![pattern],
                    |row| {
                        let active = count_from_i64(row.get::<_, i64>(0)?, 0)?;
                        let archived = count_from_i64(row.get::<_, i64>(1)?, 1)?;
                        Ok(ChatSessionCounts { active, archived })
                    },
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn messages(&self, session_id: &str) -> Result<Vec<ChatMessage>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, session_id, role, content, trace_id, created_at
                   FROM chat_messages
                  WHERE session_id = ?1
                  ORDER BY created_at ASC, id ASC",
            )?;
            let rows = statement.query_map(params![session_id], row_to_message)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn append_message(&self, message: &ChatMessage) -> Result<()> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "INSERT INTO chat_messages (
                    id, session_id, role, content, trace_id, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    message.id,
                    message.session_id,
                    message.role.as_str(),
                    message.content,
                    message.trace_id,
                    message.created_at,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            let updated = connection.execute(
                "UPDATE chat_sessions SET updated_at = ?2 WHERE id = ?1",
                params![message.session_id, message.created_at],
            )?;
            if updated == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn set_archived(&self, id: &str, archived_at: Option<&str>) -> Result<ChatSession> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE chat_sessions SET archived_at = ?2 WHERE id = ?1",
                params![id, archived_at],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            let sql = session_query("WHERE s.id = ?1");
            connection
                .query_row(&sql, params![id], row_to_session)
                .map_err(DatabaseError::from)
        })
    }

    /// Rename a session. **Deliberately does not touch `updated_at`.**
    ///
    /// The list is ordered by `updated_at`, so bumping it here would make a rename look
    /// like activity and jump the row to the top. `created_at` / `updated_at` describe the
    /// conversation; the title is a label on it.
    pub fn rename(&self, id: &str, title: &str) -> Result<ChatSession> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE chat_sessions SET title = ?2 WHERE id = ?1",
                params![id, title],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            let sql = session_query("WHERE s.id = ?1");
            connection
                .query_row(&sql, params![id], row_to_session)
                .map_err(DatabaseError::from)
        })
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        self.db.with(|connection| {
            let changed =
                connection.execute("DELETE FROM chat_sessions WHERE id = ?1", params![id])?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }
}

const SESSION_SELECT: &str =
    "SELECT s.id, s.workspace_id, s.server_id, s.title, s.created_at, s.updated_at,
                s.archived_at, COUNT(m.id),
                (SELECT substr(last_message.content, 1, 240)
                   FROM chat_messages last_message
                  WHERE last_message.session_id = s.id
                  ORDER BY last_message.created_at DESC, last_message.id DESC
                  LIMIT 1)
           FROM chat_sessions s
           LEFT JOIN chat_messages m ON m.session_id = s.id";

fn session_query(where_clause: &str) -> String {
    format!("{SESSION_SELECT} {where_clause} GROUP BY s.id")
}

fn row_to_session(row: &Row<'_>) -> rusqlite::Result<ChatSession> {
    let message_count = row
        .get::<_, i64>(7)?
        .try_into()
        .map_err(|_| decode_error(7, "message count out of u32 range"))?;
    Ok(ChatSession {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        server_id: row.get(2)?,
        title: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        archived_at: row.get(6)?,
        message_count,
        last_message_preview: row.get::<_, Option<String>>(8)?.map(preview),
    })
}

/// `COUNT(*)` is an i64 on the wire; a count that does not fit in u32 means the table is
/// not the one this build wrote, so it fails the read instead of wrapping the number the
/// filter control will display.
fn count_from_i64(value: i64, column: usize) -> rusqlite::Result<u32> {
    value
        .try_into()
        .map_err(|_| decode_error(column, "chat session count out of u32 range"))
}

fn row_to_message(row: &Row<'_>) -> rusqlite::Result<ChatMessage> {
    let role = row.get::<_, String>(2)?;
    Ok(ChatMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        role: ChatMessageRole::from_db(&role)
            .ok_or_else(|| decode_error(2, "unknown chat message role"))?,
        content: row.get(3)?,
        trace_id: row.get(4)?,
        created_at: row.get(5)?,
    })
}

fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

fn preview(value: String) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

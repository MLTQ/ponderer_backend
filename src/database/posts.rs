use anyhow::Result;
use chrono::{DateTime, Utc};
use rusqlite::params;
use serde::{Deserialize, Serialize};

use super::AgentDatabase;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportantPost {
    pub id: String,
    pub post_id: String,
    pub thread_id: String,
    pub post_body: String,
    pub why_important: String,
    pub importance_score: f64, // 0.0-1.0, how formative this experience was
    pub marked_at: DateTime<Utc>,
}

impl AgentDatabase {
    /// Save an important post
    pub fn save_important_post(&self, post: &ImportantPost) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute(
            "INSERT OR REPLACE INTO important_posts (id, post_id, thread_id, post_body, why_important, importance_score, marked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                post.id,
                post.post_id,
                post.thread_id,
                post.post_body,
                post.why_important,
                post.importance_score,
                post.marked_at.to_rfc3339()
            ],
        )?;
        Ok(())
    }

    /// Get the N most recent important posts
    pub fn get_recent_important_posts(&self, limit: usize) -> Result<Vec<ImportantPost>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, post_id, thread_id, post_body, why_important, importance_score, marked_at
             FROM important_posts
             ORDER BY marked_at DESC
             LIMIT ?1",
        )?;

        let posts = stmt
            .query_map([limit], |row| {
                Ok(ImportantPost {
                    id: row.get(0)?,
                    post_id: row.get(1)?,
                    thread_id: row.get(2)?,
                    post_body: row.get(3)?,
                    why_important: row.get(4)?,
                    importance_score: row.get(5)?,
                    marked_at: row.get::<_, String>(6)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(posts)
    }

    /// Count total important posts
    pub fn count_important_posts(&self) -> Result<usize> {
        let conn = self.lock_conn()?;
        let count: i64 =
            conn.query_row("SELECT COUNT(*) FROM important_posts", [], |row| row.get(0))?;
        Ok(count as usize)
    }

    /// Get all important posts ordered by score (lowest first)
    pub fn get_all_important_posts_by_score(&self) -> Result<Vec<ImportantPost>> {
        let conn = self.lock_conn()?;
        let mut stmt = conn.prepare(
            "SELECT id, post_id, thread_id, post_body, why_important, importance_score, marked_at
             FROM important_posts
             ORDER BY importance_score ASC",
        )?;

        let posts = stmt
            .query_map([], |row| {
                Ok(ImportantPost {
                    id: row.get(0)?,
                    post_id: row.get(1)?,
                    thread_id: row.get(2)?,
                    post_body: row.get(3)?,
                    why_important: row.get(4)?,
                    importance_score: row.get(5)?,
                    marked_at: row.get::<_, String>(6)?.parse().map_err(|e| {
                        rusqlite::Error::FromSqlConversionFailure(
                            6,
                            rusqlite::types::Type::Text,
                            Box::new(e),
                        )
                    })?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;

        Ok(posts)
    }

    /// Delete an important post by ID
    pub fn delete_important_post(&self, id: &str) -> Result<()> {
        let conn = self.lock_conn()?;
        conn.execute("DELETE FROM important_posts WHERE id = ?1", [id])?;
        Ok(())
    }
}

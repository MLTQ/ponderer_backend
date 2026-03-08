# database/posts.rs

## Purpose
`ImportantPost` type and CRUD methods for recording forum posts the agent found significant.

## Components

### `ImportantPost`
- **Does**: Records a forum post with `importance_score` (0.0-1.0) and `why_important` explanation
- **Interacts with**: `agent::reasoning` (marks posts), UI persona/memory views

### Important post methods on `AgentDatabase`
- `save_important_post` — upserts a post record
- `get_recent_important_posts` — returns N most recent posts by `marked_at` desc
- `count_important_posts` — returns total count
- `get_all_important_posts_by_score` — returns all posts ordered by `importance_score` asc (lowest-impact first, useful for eviction)
- `delete_important_post` — removes a post by ID

## Notes
- The `idx_important_posts_marked_at` index supports fast recency queries
- `importance_score` is a REAL column (0.0-1.0) representing how formative the experience was; lower scores are candidates for eviction

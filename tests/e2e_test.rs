// 笔尖APP - 集成测试

#[cfg(test)]
mod tests {
    use sqlx::sqlite::SqlitePoolOptions;
    use sqlx::Row;
    use uuid::Uuid;

    async fn setup_test_db() -> sqlx::Pool<sqlx::Sqlite> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .expect("Failed to create test db");

        sqlx::query("PRAGMA journal_mode=WAL;")
            .execute(&pool)
            .await
            .ok();

        sqlx::query(include_str!("../src/schema.sql"))
            .execute(&pool)
            .await
            .expect("Failed to init schema");

        pool
    }

    #[tokio::test]
    async fn test_database_init() {
        let pool = setup_test_db().await;
        
        let tables: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
        )
        .fetch_all(&pool)
        .await
        .unwrap();
        
        assert!(tables.contains(&"sessions".to_string()));
        assert!(tables.contains(&"messages".to_string()));
        assert!(tables.contains(&"notes".to_string()));
        assert!(tables.contains(&"meetings".to_string()));
        assert!(tables.contains(&"chunks".to_string()));
        assert!(tables.contains(&"notes_fts".to_string()));
    }

    #[tokio::test]
    async fn test_create_chat_session() {
        let pool = setup_test_db().await;
        
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO sessions (id, title, role) VALUES (?, ?, ?)")
            .bind(&id)
            .bind("测试会话")
            .bind("chat")
            .execute(&pool)
            .await
            .unwrap();
        
        let row = sqlx::query("SELECT id, title, role FROM sessions WHERE id = ?")
            .bind(&id)
            .fetch_one(&pool)
            .await
            .unwrap();
        
        assert_eq!(row.get::<String, _>("title"), "测试会话");
        assert_eq!(row.get::<String, _>("role"), "chat");
    }

    #[tokio::test]
    async fn test_create_and_search_note() {
        let pool = setup_test_db().await;
        
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        
        sqlx::query("INSERT INTO notes (id, title, content, tags, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
            .bind(&id)
            .bind("测试笔记")
            .bind("# 标题\n内容")
            .bind("测试,笔记")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();
        
        // FTS5 trigram 分词器要求查询至少 3 个字符
        let results = sqlx::query("SELECT title FROM notes_fts WHERE notes_fts MATCH ?")
            .bind("测试笔记")
            .fetch_all(&pool)
            .await
            .unwrap();
        
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].get::<String, _>("title"), "测试笔记");
    }

    #[tokio::test]
    async fn test_meeting_transcript_flow() {
        let pool = setup_test_db().await;
        
        let mid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO meetings (id, title, status, created_at, updated_at) VALUES (?, ?, 'pending', ?, ?)")
            .bind(&mid)
            .bind("测试会议")
            .bind(&now)
            .bind(&now)
            .execute(&pool)
            .await
            .unwrap();
        
        for i in 0..5 {
            let cid = Uuid::new_v4().to_string();
            sqlx::query("INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count) VALUES (?, ?, ?, ?, ?, 'unknown', 0.9, 10)")
                .bind(&cid)
                .bind(&mid)
                .bind(i as f64 * 20.0)
                .bind((i + 1) as f64 * 20.0)
                .bind(format!("这是第{}段转写内容", i))
                .execute(&pool)
                .await
                .unwrap();
        }
        
        let chunks = sqlx::query("SELECT id, transcript FROM chunks WHERE meeting_id = ? ORDER BY start_time")
            .bind(&mid)
            .fetch_all(&pool)
            .await
            .unwrap();
        
        assert_eq!(chunks.len(), 5);
        assert_eq!(chunks[0].get::<String, _>("transcript"), "这是第0段转写内容");
    }

    #[tokio::test]
    async fn test_edit_transcript_cleanup() {
        let pool = setup_test_db().await;
        
        let mid = Uuid::new_v4().to_string();
        let cid = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        
        sqlx::query("INSERT INTO meetings (id, title, status, created_at, updated_at) VALUES (?, ?, 'transcribed', ?, ?)")
            .bind(&mid).bind("测试").bind(&now).bind(&now)
            .execute(&pool).await.unwrap();
        
        sqlx::query("INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count) VALUES (?, ?, 0, 20, '原始内容', 'unknown', 0.9, 4)")
            .bind(&cid).bind(&mid)
            .execute(&pool).await.unwrap();
        
        let sid = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO chunk_summaries (id, chunk_id, content) VALUES (?, ?, ?)")
            .bind(&sid).bind(&cid).bind("旧摘要")
            .execute(&pool).await.unwrap();
        
        let fid = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO final_summaries (id, meeting_id, content) VALUES (?, ?, ?)")
            .bind(&fid).bind(&mid).bind("旧纪要")
            .execute(&pool).await.unwrap();
        
        // 编辑片段
        sqlx::query("UPDATE chunks SET transcript = '修改后内容', word_count = 5, processed_flag = 0 WHERE id = ? AND meeting_id = ?")
            .bind(&cid).bind(&mid)
            .execute(&pool).await.unwrap();
        
        sqlx::query("DELETE FROM chunk_summaries WHERE chunk_id = ?")
            .bind(&cid).execute(&pool).await.unwrap();
        sqlx::query("DELETE FROM final_summaries WHERE meeting_id = ?")
            .bind(&mid).execute(&pool).await.unwrap();
        
        let chunk = sqlx::query("SELECT transcript, processed_flag FROM chunks WHERE id = ?")
            .bind(&cid).fetch_one(&pool).await.unwrap();
        assert_eq!(chunk.get::<String, _>("transcript"), "修改后内容");
        assert_eq!(chunk.get::<i64, _>("processed_flag"), 0);
        
        let summary_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM chunk_summaries").fetch_one(&pool).await.unwrap();
        assert_eq!(summary_count, 0);
    }
}

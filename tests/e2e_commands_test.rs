// 笔尖APP - 端到端 Command 功能测试

#[cfg(test)]
mod tests {
    use bijian::state::AppState;
    use tauri::Manager;

    async fn setup_app() -> tauri::App<tauri::test::MockRuntime> {
        std::env::set_var("PANNOTE_LOG_TO_FILE", "0"); // v2.6.0 测试不写生产日志
        // v2.3.2: 测试默认激活 Pro，否则 trigger_aggregate 等命令被门控拒绝
        bijian::feature::set_tier(bijian::feature::FeatureTier::Pro, None);
        let db_path = format!("/tmp/bijian_test_{}.db", uuid::Uuid::new_v4());
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-shm", db_path));
        let _ = std::fs::remove_file(format!("{}-wal", db_path));
        
        let state = AppState::new(db_path).await.expect("Failed to create AppState");
        
        tauri::test::mock_builder()
            .manage(state)
            .invoke_handler(tauri::generate_handler![
                bijian::commands::health_check,
                bijian::commands::create_chat_session,
                bijian::commands::list_chat_sessions,
                bijian::commands::get_chat_session,
                bijian::commands::send_message,
                bijian::commands::rate_message,
                bijian::commands::delete_chat_session,
                bijian::commands::distill_evolution,
                bijian::commands::get_evolution_insights,
                bijian::commands::create_note,
                bijian::commands::list_notes,
                bijian::commands::get_note,
                bijian::commands::update_note,
                bijian::commands::delete_note,
                bijian::commands::search,
                bijian::commands::create_meeting,
                bijian::commands::list_meetings,
                bijian::commands::get_meeting,
                bijian::commands::upload_audio,
                bijian::commands::trigger_aggregate,
                bijian::commands::get_summary,
                bijian::commands::get_transcript,
                bijian::commands::edit_transcript_chunk,
                bijian::commands::asr_status,
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    // ========== 健康检查 ==========
    #[tokio::test]
    async fn test_health_check() {
        let app = setup_app().await;
        let s = app.state::<AppState>();
        let result = bijian::commands::health_check(s.clone()).await;
        assert!(result.is_ok());
        let health = result.unwrap();
        assert_eq!(health.status, "ok");
        assert!(health.services.get("database").is_some());
    }

    // ========== AI 对话模块 ==========
    #[tokio::test]
    async fn test_chat_session_crud() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        let session = bijian::commands::create_chat_session(s.clone(), Some("测试会话".to_string()), Some("chat".to_string())).await.unwrap();
        assert_eq!(session.title, "测试会话");
        assert_eq!(session.role, "chat");

        let sessions = bijian::commands::list_chat_sessions(s.clone()).await.unwrap();
        assert_eq!(sessions.len(), 1);

        let detail = bijian::commands::get_chat_session(s.clone(), session.id.clone()).await.unwrap();
        assert!(detail.get("session").is_some());

        let msg = bijian::commands::send_message(s.clone(), session.id.clone(), "你好".to_string(), None, None, None).await.unwrap();
        assert!(!msg.message_id.is_empty());

        let result = bijian::commands::delete_chat_session(s.clone(), session.id).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_message_rating() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        let session = bijian::commands::create_chat_session(s.clone(), None, None).await.unwrap();
        let msg = bijian::commands::send_message(s.clone(), session.id, "测试".to_string(), None, None, None).await.unwrap();
        
        let result = bijian::commands::rate_message(s.clone(), msg.message_id, 1).await;
        assert!(result.is_ok());
    }

    // ========== 笔记模块 ==========
    #[tokio::test]
    async fn test_note_crud() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        let note = bijian::commands::create_note(
            s.clone(),
            "我的笔记".to_string(),
            "这是笔记内容".to_string(),
            Some("标签1,标签2".to_string()),
        ).await.unwrap();
        assert_eq!(note.title, "我的笔记");
        assert_eq!(note.tags, "标签1,标签2");

        let notes = bijian::commands::list_notes(s.clone()).await.unwrap();
        assert_eq!(notes.len(), 1);

        let fetched = bijian::commands::get_note(s.clone(), note.id.clone()).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().title, "我的笔记");

        let result = bijian::commands::update_note(
            s.clone(),
            note.id.clone(),
            Some("更新标题".to_string()),
            Some("更新内容".to_string()),
            None,
        ).await;
        if let Err(ref e) = result {
            eprintln!("update_note error: {:?}", e);
        }
        assert!(result.is_ok());

        let result = bijian::commands::delete_note(s.clone(), note.id).await;
        assert!(result.is_ok());

        let notes = bijian::commands::list_notes(s.clone()).await.unwrap();
        assert_eq!(notes.len(), 0);
    }

    #[tokio::test]
    async fn test_note_search() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        bijian::commands::create_note(s.clone(), "Rust 编程".to_string(), "学习 Rust 语言".to_string(), Some("编程".to_string())).await.unwrap();
        bijian::commands::create_note(s.clone(), "Python 编程".to_string(), "学习 Python 语言".to_string(), Some("编程".to_string())).await.unwrap();
        bijian::commands::create_note(s.clone(), "购物清单".to_string(), "牛奶、面包、鸡蛋".to_string(), Some("生活".to_string())).await.unwrap();

        // FTS5 trigram 分词器要求查询至少 3 个字符
        // 搜索 "Rust" 出现在标题和内容中
        let results = bijian::commands::search(s.clone(), "Rust".to_string(), None).await.unwrap();
        assert!(results.len() >= 1);
    }

    // ========== 会议模块 ==========
    #[tokio::test]
    async fn test_meeting_crud() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        let meeting = bijian::commands::create_meeting(s.clone(), Some("周会".to_string()), None).await.unwrap();
        assert_eq!(meeting.title, "周会");
        assert_eq!(meeting.status, "pending");

        let meetings = bijian::commands::list_meetings(s.clone()).await.unwrap();
        assert_eq!(meetings.len(), 1);

        let fetched = bijian::commands::get_meeting(s.clone(), meeting.id.clone()).await.unwrap();
        assert!(fetched.is_some());

        let chunks = bijian::commands::get_transcript(s.clone(), meeting.id.clone()).await.unwrap();
        assert_eq!(chunks.len(), 0);

        let result = bijian::commands::trigger_aggregate(s.clone(), meeting.id.clone(), None, None, None).await;
        assert!(result.is_ok());

        // v2.3.2: trigger_aggregate 只入 job 队列，worker 异步消费，
        // 测试瞬时无 summary 可读，符合预期——验证 job 创建成功即可
        assert!(result.is_ok());
    }

    // ========== 进化系统 ==========
    #[tokio::test]
    async fn test_evolution_system() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        let result = bijian::commands::distill_evolution(s.clone()).await.unwrap();
        assert!(result.contains("暂无足够"));

        let session = bijian::commands::create_chat_session(s.clone(), None, None).await.unwrap();
        let msg = bijian::commands::send_message(s.clone(), session.id, "测试".to_string(), None, None, None).await.unwrap();
        bijian::commands::rate_message(s.clone(), msg.message_id, 1).await.unwrap();

        let result = bijian::commands::distill_evolution(s.clone()).await.unwrap();
        assert!(result.contains("蒸馏完成"));

        let insights = bijian::commands::get_evolution_insights(s.clone()).await.unwrap();
        assert!(insights.len() >= 1);
    }

    // ========== ASR 状态 ==========
    #[tokio::test]
    async fn test_asr_status() {
        let app = setup_app().await;
        let s = app.state::<AppState>();
        let result = bijian::commands::asr_status(s.clone()).await;
        assert!(result.is_ok());
        let status = result.unwrap();
        assert_eq!(status.primary, "firered");
    }
}

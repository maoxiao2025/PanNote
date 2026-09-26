// 笔尖APP - 全面功能压实测试

#[cfg(test)]
mod tests {
    use bijian::commands::*;
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
                bijian::commands::asr_ensure_running,
            ])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    // ========== 1. AI 对话模块 ==========
    #[tokio::test]
    async fn test_ai_chat_full_flow() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 创建会话
        let session = create_chat_session(s.clone(), Some("AI测试会话".to_string()), Some("chat".to_string())).await.unwrap();
        assert!(!session.id.is_empty());
        assert_eq!(session.title, "AI测试会话");

        // 发送消息
        let msg = send_message(s.clone(), session.id.clone(), "你好，请简单回复".to_string(), None, None, None).await.unwrap();
        assert!(!msg.message_id.is_empty());

        // 获取会话详情
        let detail = get_chat_session(s.clone(), session.id.clone()).await.unwrap();
        assert!(detail.get("session").is_some());
        let messages = detail.get("messages").unwrap().as_array().unwrap();
        assert_eq!(messages.len(), 1); // 只有 user 消息

        // 评分
        rate_message(s.clone(), msg.message_id, 1).await.unwrap();

        // 列出会话
        let sessions = list_chat_sessions(s.clone()).await.unwrap();
        assert_eq!(sessions.len(), 1);

        // 删除会话
        delete_chat_session(s.clone(), session.id).await.unwrap();
        let sessions = list_chat_sessions(s.clone()).await.unwrap();
        assert_eq!(sessions.len(), 0);
    }

    // ========== 2. 笔记模块 ==========
    #[tokio::test]
    async fn test_notes_full_flow() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 创建笔记
        let note = create_note(
            s.clone(),
            "测试笔记".to_string(),
            "这是测试内容，包含一些文字。".to_string(),
            Some("测试,笔记".to_string()),
        ).await.unwrap();
        assert!(!note.id.is_empty());
        assert_eq!(note.title, "测试笔记");

        // 列出笔记
        let notes = list_notes(s.clone()).await.unwrap();
        assert_eq!(notes.len(), 1);

        // 获取笔记
        let fetched = get_note(s.clone(), note.id.clone()).await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().title, "测试笔记");

        // 更新笔记
        update_note(
            s.clone(),
            note.id.clone(),
            Some("更新后的标题".to_string()),
            Some("更新后的内容".to_string()),
            Some("新标签".to_string()),
        ).await.unwrap();

        let updated = get_note(s.clone(), note.id.clone()).await.unwrap().unwrap();
        assert_eq!(updated.title, "更新后的标题");
        assert_eq!(updated.content, "更新后的内容");
        assert_eq!(updated.tags, "新标签");

        // 搜索笔记
        let results = search(s.clone(), "更新后的标题".to_string(), None).await.unwrap();
        assert!(results.len() >= 1);

        // 删除笔记
        delete_note(s.clone(), note.id).await.unwrap();
        let notes = list_notes(s.clone()).await.unwrap();
        assert_eq!(notes.len(), 0);
    }

    // ========== 3. 会议模块 ==========
    #[tokio::test]
    async fn test_meeting_full_flow() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 创建会议（v2.5.0 起 create_meeting 增加 note_id 第三参数，测试补 None）
        let meeting = create_meeting(s.clone(), Some("测试会议".to_string()), None).await.unwrap();
        assert!(!meeting.id.is_empty());
        assert_eq!(meeting.status, "pending");

        // 列出会议
        let meetings = list_meetings(s.clone()).await.unwrap();
        assert_eq!(meetings.len(), 1);

        // 获取会议
        let fetched = get_meeting(s.clone(), meeting.id.clone()).await.unwrap();
        assert!(fetched.is_some());

        // 获取转写（空）
        let chunks = get_transcript(s.clone(), meeting.id.clone()).await.unwrap();
        assert_eq!(chunks.len(), 0);

        // 触发汇总
        let result = trigger_aggregate(s.clone(), meeting.id.clone(), None, None, None).await;
        assert!(result.is_ok());

        // 获取纪要（空 - 刚 trigger_aggregate 尚未完成，应返回 Err）
        let summary_result = get_summary(s.clone(), meeting.id).await;
        assert!(summary_result.is_err()); // 空会议 + 未触发 LLM 汇总 会返回 Err
    }

    // ========== 4. 进化系统 ==========
    #[tokio::test]
    async fn test_evolution_full_flow() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 无消息时蒸馏
        let result = distill_evolution(s.clone()).await.unwrap();
        assert!(result.contains("暂无足够"));

        // 创建会话和消息
        let session = create_chat_session(s.clone(), None, None).await.unwrap();
        let msg = send_message(s.clone(), session.id, "测试消息".to_string(), None, None, None).await.unwrap();
        rate_message(s.clone(), msg.message_id, 1).await.unwrap();

        // 蒸馏
        let result = distill_evolution(s.clone()).await.unwrap();
        assert!(result.contains("蒸馏完成"));

        // 获取洞察
        let insights = get_evolution_insights(s.clone()).await.unwrap();
        assert!(insights.len() >= 1);
    }

    // ========== 5. ASR 模块 ==========
    #[tokio::test]
    async fn test_asr_status() {
        let app = setup_app().await;
        let s = app.state::<AppState>();
        let result = asr_status(s.clone()).await;
        assert!(result.is_ok());
        let status = result.unwrap();
        assert_eq!(status.primary, "firered");
    }

    // ========== 6. 健康检查 ==========
    #[tokio::test]
    async fn test_health_check() {
        let app = setup_app().await;
        let s = app.state::<AppState>();
        let result = health_check(s.clone()).await;
        assert!(result.is_ok());
        let health = result.unwrap();
        assert_eq!(health.status, "ok");
    }

    // ========== 7. 搜索功能 ==========
    #[tokio::test]
    async fn test_search_functionality() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 创建多篇笔记
        create_note(s.clone(), "Rust 编程笔记".to_string(), "学习 Rust 语言".to_string(), Some("编程".to_string())).await.unwrap();
        create_note(s.clone(), "Python 编程笔记".to_string(), "学习 Python 语言".to_string(), Some("编程".to_string())).await.unwrap();
        create_note(s.clone(), "购物清单".to_string(), "牛奶、面包、鸡蛋".to_string(), Some("生活".to_string())).await.unwrap();

        // 搜索（FTS5 trigram 分词器要求查询至少 3 个字符）
        let results = search(s.clone(), "编程笔记".to_string(), None).await.unwrap();
        assert!(results.len() >= 1);
    }

    // ========== 8. 数据库完整性 ==========
    #[tokio::test]
    async fn test_database_integrity() {
        let app = setup_app().await;
        let s = app.state::<AppState>();

        // 创建各种数据
        let session = create_chat_session(s.clone(), Some("会话".to_string()), None).await.unwrap();
        send_message(s.clone(), session.id, "消息".to_string(), None, None, None).await.unwrap();
        create_note(s.clone(), "笔记".to_string(), "内容".to_string(), None).await.unwrap();
        create_meeting(s.clone(), Some("会议".to_string()), None).await.unwrap();

        // 验证数据完整性
        let sessions = list_chat_sessions(s.clone()).await.unwrap();
        assert_eq!(sessions.len(), 1);

        let notes = list_notes(s.clone()).await.unwrap();
        assert_eq!(notes.len(), 1);

        let meetings = list_meetings(s.clone()).await.unwrap();
        assert_eq!(meetings.len(), 1);
    }
}

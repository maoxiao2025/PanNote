// PanNote 状态机冒烟测试集（v2.6.0，改版必跑）
// 定位：9-24 事故的核心教训区——"状态必须等于事实"。每次改版跑一遍，5 分钟内出通过/失败报告。
// 覆盖：全成功/部分失败/全静音/进行中四类状态收敛、导入型 0 行保护、
//       巡检幂等、stale processing 段接管、段唯一约束、纪要段级门槛（failed 默认拒/force 放行）。
// 不依赖 ASR/Ollama（纯 DB 层状态机），音频差集用临时目录伪 WAV 文件。

#[cfg(test)]
mod smoke {
    use bijian::commands::*;
    use bijian::state::AppState;
    use sqlx::Row;
    use tauri::Manager;

    async fn setup() -> tauri::App<tauri::test::MockRuntime> {
        std::env::set_var("PANNOTE_LOG_TO_FILE", "0"); // v2.6.0 测试不写生产日志
        bijian::feature::set_tier(bijian::feature::FeatureTier::Pro, None);
        let db_path = format!("/tmp/pannote_smoke_{}.db", uuid::Uuid::new_v4());
        let _ = std::fs::remove_file(&db_path);
        let _ = std::fs::remove_file(format!("{}-shm", &db_path));
        let _ = std::fs::remove_file(format!("{}-wal", &db_path));
        let state = AppState::new(db_path).await.expect("AppState 创建失败");
        tauri::test::mock_builder()
            .manage(state)
            .invoke_handler(tauri::generate_handler![])
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap()
    }

    /// 建会议并按剧本落段行
    async fn make_meeting_with_segments<R: tauri::Runtime>(
        db: &bijian::db::Database,
        ah: &tauri::AppHandle<R>,
        title: &str,
        segs: &[(&str, u32)],
    ) -> String {
        let mid = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        sqlx::query("INSERT INTO meetings (id, title, status, duration, created_at, updated_at, session_dir, note_id) VALUES (?, ?, 'pending', 0, ?, ?, '', '')")
            .bind(&mid).bind(title).bind(&now).bind(&now)
            .execute(db.pool()).await.unwrap();
        for (state, idx) in segs {
            upsert_segment_state(db, &mid, *idx, state, "").await;
        }
        let _ = ah;
        mid
    }

    fn db_of(app: &tauri::App<tauri::test::MockRuntime>) -> std::sync::Arc<bijian::db::Database> {
        app.state::<AppState>().db.clone()
    }

    async fn meeting_stats(db: &sqlx::Pool<sqlx::Sqlite>, mid: &str) -> (String, i64, i64, i64) {
        let r = sqlx::query("SELECT status, expected_segments, completed_segments, failed_segments FROM meetings WHERE id = ?")
            .bind(mid).fetch_one(db).await.unwrap();
        (r.get::<String, _>("status"), r.get::<i64, _>("expected_segments"), r.get::<i64, _>("completed_segments"), r.get::<i64, _>("failed_segments"))
    }

    // ===== 1. 全成功 → transcribed，统计对齐（9-25 修复的"1/8 标 transcribed"回归线）=====
    #[tokio::test]
    async fn smoke_all_done_transcribed() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let segs: Vec<(&str, u32)> = (0..8).map(|i| ("done", i)).collect();
        let mid = make_meeting_with_segments(&db, &ah, "全成功会议", &segs).await;
        update_meeting_status_by_segments(&db, &ah, &mid, true).await;
        let (status, exp, done, failed) = meeting_stats(db.pool(), &mid).await;
        assert_eq!(status, "transcribed");
        assert_eq!((exp, done, failed), (8, 8, 0), "expected/completed/failed 必须与段级表对齐");
    }

    // ===== 2. 部分失败 → transcription_partial（不冒充成功）=====
    #[tokio::test]
    async fn smoke_partial_failed_not_fake_success() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mut segs: Vec<(&str, u32)> = (0..7).map(|i| ("done", i)).collect();
        segs.push(("failed", 7));
        let mid = make_meeting_with_segments(&db, &ah, "部分失败会议", &segs).await;
        update_meeting_status_by_segments(&db, &ah, &mid, true).await;
        let (status, exp, done, failed) = meeting_stats(db.pool(), &mid).await;
        assert_eq!(status, "transcription_partial");
        assert_eq!((exp, done, failed), (8, 7, 1));
    }

    // ===== 3. 全静音 → no_voice（无声 ≠ 失败，区分两种事实）=====
    #[tokio::test]
    async fn smoke_all_silent_no_voice() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let segs: Vec<(&str, u32)> = (0..56).map(|i| ("silent", i)).collect();
        let mid = make_meeting_with_segments(&db, &ah, "全零会议", &segs).await;
        update_meeting_status_by_segments(&db, &ah, &mid, true).await;
        let (status, exp, done, _) = meeting_stats(db.pool(), &mid).await;
        assert_eq!(status, "no_voice");
        assert_eq!((exp, done), (56, 0));
    }

    // ===== 4. 有未完成段 → 停留 transcribing，不越终态 =====
    #[tokio::test]
    async fn smoke_pending_stays_transcribing() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mut segs: Vec<(&str, u32)> = (0..5).map(|i| ("done", i)).collect();
        segs.push(("pending", 5));
        let mid = make_meeting_with_segments(&db, &ah, "进行中会议", &segs).await;
        update_meeting_status_by_segments(&db, &ah, &mid, true).await;
        let (status, _, done, _) = meeting_stats(db.pool(), &mid).await;
        assert_eq!(status, "transcribing", "有 pending 段时绝不收敛到任何终态");
        assert_eq!(done, 5);
    }

    // ===== 5. 导入型 0 行保护：无段级事实不据此改状态（v2.5.1 关键修复回归线）=====
    #[tokio::test]
    async fn smoke_import_zero_row_protection() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mid = uuid::Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        // 导入型：直接标 transcribed，段级表 0 行
        sqlx::query("INSERT INTO meetings (id, title, status, duration, created_at, updated_at, session_dir, note_id) VALUES (?, ?, 'transcribed', 60, ?, ?, '', '')")
            .bind(&mid).bind("音频导入会议").bind(&now).bind(&now)
            .execute(db.pool()).await.unwrap();
        update_meeting_status_by_segments(&db, &ah, &mid, true).await;
        let (status, _, _, _) = meeting_stats(db.pool(), &mid).await;
        assert_eq!(status, "transcribed", "0 行保护：导入型会议不被误标 failed");
    }

    // ===== 6. 巡检幂等：跑两遍统计不变（防回填引入重复副作用）=====
    #[tokio::test]
    async fn smoke_consistency_check_idempotent() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mut segs: Vec<(&str, u32)> = (0..3).map(|i| ("done", i)).collect();
        segs.push(("failed", 3));
        let mid = make_meeting_with_segments(&db, &ah, "巡检幂等", &segs).await;
        // 先人为弄脏主表
        sqlx::query("UPDATE meetings SET expected_segments = -1, completed_segments = 0, failed_segments = 0 WHERE id = ?")
            .bind(&mid).execute(db.pool()).await.unwrap();
        consistency_check_and_repair(&db, &ah).await;
        let s1 = meeting_stats(db.pool(), &mid).await;
        consistency_check_and_repair(&db, &ah).await; // 第二遍
        let s2 = meeting_stats(db.pool(), &mid).await;
        assert_eq!(s1, s2, "巡检必须幂等");
        assert_eq!(s1.1, 4, "expected 已按段级表回填");
    }

    // ===== 7. stale processing 段（>10 分钟）被差集重新接管（9-25 巡检接管逻辑回归线）=====
    #[tokio::test]
    async fn smoke_stale_processing_takeover() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mid = make_meeting_with_segments(&db, &ah, "stale 会议", &[]).await;
        // 伪 WAV 段文件（差集按文件存在扫描，不调 ASR）
        let dir = std::env::temp_dir().join(format!("pannote_smoke_stale_{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        // 段行与文件编号必须对齐：0=stale processing、1=fresh processing（租约内）、2=无段行
        for i in 0..3u32 {
            std::fs::write(dir.join(format!("seg_{}.wav", i)), b"RIFF-fake").unwrap();
        }
        let sd = dir.to_string_lossy().to_string();
        sqlx::query("UPDATE meetings SET session_dir = ? WHERE id = ?").bind(&sd).bind(&mid)
            .execute(db.pool()).await.unwrap();
        // 段 0：stale processing（started_at 为 1 小时前）
        upsert_segment_state(&db, &mid, 0, "processing", "").await;
        sqlx::query("UPDATE transcription_segments SET started_at = ? WHERE meeting_id = ? AND segment_index = 0")
            .bind(chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()).bind(&mid)
            .execute(db.pool()).await.unwrap();
        sqlx::query("UPDATE transcription_segments SET started_at = datetime('now', '-1 hour') WHERE meeting_id = ? AND segment_index = 0")
            .bind(&mid).execute(db.pool()).await.unwrap();
        // 段 1：新鲜 processing（10 分钟内，租约保护不碰）
        upsert_segment_state(&db, &mid, 1, "processing", "").await;
        sqlx::query("UPDATE transcription_segments SET started_at = datetime('now') WHERE meeting_id = ? AND segment_index = 1")
            .bind(&mid).execute(db.pool()).await.unwrap();
        let missing = diff_missing_segments(&db, &mid, &sd).await;
        assert!(missing.contains(&0), "stale processing 段（1 小时前）必须被接管");
        assert!(!missing.contains(&1), "新鲜 processing 段（租约内）不碰");
        assert!(missing.contains(&2), "无段行文件必须进差集");
        std::fs::remove_dir_all(&dir).ok();
    }

    // ===== 8. 段唯一约束：同 (meeting, index) 不产生重复行 =====
    #[tokio::test]
    async fn smoke_segment_unique_constraint() {
        let app = setup().await;
        let db = db_of(&app);
        let ah = app.handle();
        let mid = make_meeting_with_segments(&db, &ah, "唯一约束", &[]).await;
        upsert_segment_state(&db, &mid, 0, "pending", "").await;
        upsert_segment_state(&db, &mid, 0, "done", "").await; // upsert 不炸且只一行
        let n: i64 = sqlx::query("SELECT COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? AND segment_index = 0")
            .bind(&mid).fetch_one(db.pool()).await.unwrap().get("c");
        assert_eq!(n, 1, "upsert 幂等，(meeting_id, segment_index) 唯一");
    }

    // ===== 9. 纪要段级门槛：failed 默认拒绝，force 放行（v2.5.2 门槛回归线）=====
    #[tokio::test]
    async fn smoke_aggregate_gate_and_force() {
        let app = setup().await;
        let s = app.state::<AppState>();
        let ah = app.handle();
        let db = db_of(&app);
        let mid = make_meeting_with_segments(&db, &ah, "门槛会议", &[("done", 0), ("done", 1), ("failed", 2)]).await;
        // 默认拒绝
        let r = trigger_aggregate(s.clone(), mid.clone(), None, None, None).await;
        assert!(r.is_err(), "存在失败段时默认必须拒绝生成纪要");
        assert!(r.unwrap_err().contains("失败段"), "拒绝信息要能指导用户下一步动作");
        // force 放行 + partial_note 落 payload
        let ok = trigger_aggregate(s.clone(), mid.clone(), None, None, Some(true)).await.unwrap();
        assert!(ok["ok"].as_bool().unwrap());
        let payload: String = sqlx::query("SELECT payload FROM jobs WHERE meeting_id = ? AND type = 'aggregate' ORDER BY created_at DESC LIMIT 1")
            .bind(&mid).fetch_one(db.pool()).await.unwrap().get("payload");
        let v: serde_json::Value = serde_json::from_str(&payload).unwrap();
        let pn = v["partial_note"].as_str().unwrap_or("");
        assert!(pn.contains("部分转写"), "force 放行时 payload 必须带部分转写标注: {}", pn);
        // 未完成段时 force 也不放行
        let mid2 = make_meeting_with_segments(&db, &ah, "进行中门槛", &[("done", 0), ("pending", 1)]).await;
        let r2 = trigger_aggregate(s.clone(), mid2, None, None, Some(true)).await;
        assert!(r2.is_err(), "pending 未清零时无论是否 force 都拒绝");
    }

    // ===== 10. 失败段重试：failed → pending 后可被差集吃掉 =====
    #[tokio::test]
    async fn smoke_retry_failed_segments() {
        let app = setup().await;
        let s = app.state::<AppState>();
        let ah = app.handle();
        let db = db_of(&app);
        let mid = make_meeting_with_segments(&db, &ah, "重试会议", &[("done", 0), ("failed", 1), ("failed", 2)]).await;
        let r = retry_failed_segments(s.clone(), ah.clone(), mid.clone()).await.unwrap();
        assert_eq!(r["reset_count"].as_i64().unwrap(), 2, "两个失败段应被重置");
        let n: i64 = sqlx::query("SELECT COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? AND state = 'pending'")
            .bind(&mid).fetch_one(db.pool()).await.unwrap().get("c");
        assert_eq!(n, 2, "failed 全部转 pending");
        let nf: i64 = sqlx::query("SELECT COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? AND state = 'failed'")
            .bind(&mid).fetch_one(db.pool()).await.unwrap().get("c");
        assert_eq!(nf, 0);
    }

    // ===== 11. 纪要导出：组稿结构完整 + docx 是合法 zip（Word/WPS 可开）=====
    #[tokio::test]
    async fn smoke_export_summary_docx_md() {
        let summary = serde_json::json!({
            "tldr": "第三季度经营情况汇报会",
            "key_points": ["营业收入三点二亿元，同比增长百分之八"],
            "topics": [{"title": "回款专项", "summary": "期末应收四点五亿元，较年初下降百分之三"}],
            "actions": ["盯紧四季度回款目标，逐户落实责任人"],
            "partial_note": "【注意：本纪要基于部分转写生成（成功 7/8 段，另有 1 段转写失败）】",
        });
        let md = summary_json_to_markdown("季度经营分析会", "2026-09-25", &summary);
        // md 结构断言：标题、概要、结论、议题、行动项、部分转写标注全部在场
        for expect in ["# 季度经营分析会 会议纪要", "## 概要", "## 核心结论", "## 议题讨论", "### 回款专项", "## 行动项", "部分转写生成"] {
            assert!(md.contains(expect), "组稿缺节: {}\n--- md ---\n{}", expect, md);
        }
        // docx 生成 + 合法性（zip 魔数 PK = Word 可开的前提）
        let path = std::env::temp_dir().join(format!("pannote_smoke_export_{}.docx", uuid::Uuid::new_v4()));
        build_docx(&md, &path).expect("docx 生成失败");
        let head = std::fs::read(&path).unwrap();
        assert!(head.len() > 1000, "docx 过小（{} 字节）", head.len());
        assert_eq!(&head[0..2], b"PK", "docx 必须是合法 zip（Word/WPS 可打开的前提）");
        std::fs::remove_file(&path).ok();
    }
}

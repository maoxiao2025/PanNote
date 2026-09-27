// 笔尖APP - Tauri v2 库入口

pub mod agenda_detect;
pub mod asr_manager;
pub mod asr_provider;
pub mod commands;
pub mod db;
pub mod feature;
pub mod gpu_manager;
pub mod license;
pub mod logger;
pub mod models;
pub mod ollama_manager;
pub mod services;
pub mod state;

use tauri::Manager;
use state::AppState;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // v2.3.2: 全局 panic hook——至少把 panic 信息打到 stderr，便于事后排查
    // 注：catch_unwind 包 5 处 tauri::async_runtime::spawn + 2 处 std::thread::spawn
    // 工作量大且 tauri 内部已有部分兜底，留 v2.4.0 完整实现
    std::panic::set_hook(Box::new(|info| {
        eprintln!("[FATAL PANIC] {}", info);
    }));
    tauri::Builder::default()
        .setup(|app| {
            let app_handle = app.handle().clone();
            
            let db_path = app_handle
                .path()
                .app_data_dir()
                .unwrap_or_default()
                .join("history.db");

            std::fs::create_dir_all(db_path.parent().unwrap()).ok();

            let state = tauri::async_runtime::block_on(async {
                AppState::new(db_path.to_string_lossy().to_string()).await
            })?;

            // 产品化 v1.0：后台探测 ASR 引擎（嵌入式模型加载 ~5s，不能阻塞 setup）。
            // 探测完成装进全局注册表；嵌入式成功则跳过 Python 服务拉起（L1 零依赖的核心收益），
            // 失败才回退拉起 Python HTTP 服务（开发机/完整版场景）。
            {
                // 打包进 App 的模型位置：.app/Contents/Resources/models/
                if let Ok(dir) = app_handle.path().resource_dir() {
                    asr_provider::register_model_base_hint(
                        dir.join("models").to_string_lossy().to_string(),
                    );
                }
                let asr_mgr = state.asr_manager.clone();
                std::thread::spawn(move || {
                    let t0 = std::time::Instant::now();
                    let reg = asr_provider::AsrRegistry::detect();
                    let engine = reg.engine_name();
                    let embedded = engine == Some("embedded_paraformer");
                    let detect_log = reg.detect_log.clone();
                    asr_provider::registry_install(reg);
                    eprintln!(
                        "[lib.rs] ASR 探测完成（{}ms）：{}",
                        t0.elapsed().as_millis(),
                        engine.unwrap_or("不可用")
                    );
                    crate::logger::log("Startup", &format!("ASR 引擎探测：{}", detect_log));
                    if !embedded {
                        // 嵌入式未就绪 → 回退旧链路：后台预热 Python ASR 服务
                        asr_mgr.start_in_background();
                    }
                });
            }

            // v2.4.5: GPU router 默认启用（恢复 v2.4.0 设计）。
            // 9/20 穿测硬数据：GPU gen 30tps >> CPU 8.4tps（AMD 5300M + Vulkan/MoltenVK 路线），
            // router v6 按"gen密集→GPU / prompt密集→CPU / 热谷→CPU散热"智能分流。
            // 9/21 崩溃循环（load 13.2）根因是健康检查 3s 超时误判（v2.4.2 已修为 8s），非 router 本身；
            // v2.4.2 曾误判"GPU 不可用"而默认关闭——特此纠正。无独显/无 GPU 服务的机器
            // router 会自动降级 Ollama CPU，行为安全；如需强制关闭：BIJIAN_GPU_ROUTER=0。
            let gpu_router_enabled = std::env::var("BIJIAN_GPU_ROUTER")
                .map(|v| !(v == "0" || v.eq_ignore_ascii_case("false")))
                .unwrap_or(true);
            if gpu_router_enabled {
                std::thread::spawn(move || {
                    let gm = std::sync::Arc::new(gpu_manager::GpuManager::new());
                    let _ = gm.start_router();
                    gm.start_keepalive();
                });
            }

            // v2.4.1: 探测并自动拉起 Ollama（app 优先，无 app 再 serve），避免「打开 PanNote 但 Ollama 是关的」
            // v2.4.5: 复用 AppState 里的 OllamaManager，启动 keepalive（含空闲自动卸载模型）
            {
                let om = state.ollama_manager.clone();
                let om_clone = om.clone();
                std::thread::spawn(move || {
                    if let Err(e) = om_clone.ensure_running() {
                        eprintln!("[lib.rs] 自动拉起 Ollama 失败: {}", e);
                    }
                });
                om.start_keepalive();
            }

            // v2.3.3: 启动时检查 launchd 自愈链路完整性——plist 存在但 wrapper 缺失则自动修复
            // 根治「Trae Code 只写代码不落盘」类问题：代码里 render_asr_agent_plist() 有完整模板，
            // 但实际没执行 asr_install_launchd() 落盘，导致 launchd 拉起失败、ASR 服务起不来。
            {
                let plist_path = commands::asr_launchd_plist_path();
                let wrapper_path = std::path::PathBuf::from(commands::asr_wrapper_path());
                if plist_path.exists() && !wrapper_path.exists() {
                    eprintln!("[lib.rs] 检测到 launchd plist 存在但 wrapper 缺失，自动修复...");
                    if let Err(e) = tauri::async_runtime::block_on(commands::asr_install_launchd()) {
                        eprintln!("[lib.rs] launchd 自愈修复失败: {}", e);
                    } else {
                        eprintln!("[lib.rs] launchd 自愈修复完成");
                    }
                }
            }

            // v2.5.0: 提前取出续转所需资源（manage 会 move state）
            // 产品化 v1.0：续转链路改走 ASR 注册表（全局），不再需要 http_client 传参
            let resume_db = state.db.clone();

            app.manage(state);

            // 加载本地激活码（如有）
            let app_data_dir = app_handle.path().app_data_dir().unwrap_or_default();
            match license::load_license(&app_data_dir) {
                Some(result) if result.success => {
                    log::info!("许可已加载: tier={:?}", result.tier);
                }
                Some(result) => {
                    log::warn!("许可加载失败: {}", result.message);
                    feature::set_tier(feature::FeatureTier::Free, None);
                }
                None => {
                    log::info!("未找到激活码，使用免费版");
                    feature::set_tier(feature::FeatureTier::Free, None);
                }
            }

            // 启动后台 Worker（消费 jobs 表的任务）
            services::worker::start_worker(app_handle.clone());

            // ===== v2.5.0 可靠性工程（9-25 落地）=====
            // 日志清理（保留 14 天）+ 启动日志
            logger::cleanup_old_logs();
            logger::log("Startup", "PanNote 启动——v2.5.0 可靠性工程：启动续转 + 巡检 + 段级持久化已启用");

            // 启动续转：扫描所有未完成会议的差集，非空则自动补转
            // 覆盖场景：app 升级替换 / 崩溃 / OOM 被杀——进程死了任务不丢（9-25 核心修复）
            {
                let db = resume_db.clone();
                let ah = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(3)).await; // 等 DB/ASR 就绪
                    // v2.5.1 一致性巡检：先收敛统计失真，再续转（统计是纪要门槛/前端进度的事实基准）
                    commands::consistency_check_and_repair(&db, &ah).await;
                    commands::resume_unfinished_transcriptions(db, &ah).await;
                });
            }

            // 定时巡检：每 5 分钟差集扫描（转写中断的兑底；含 stale processing 段接管）
            // v2.5.1：同步附带统计收敛（幂等）；v2.6.0：磁盘水位检查（>90% 归档最旧已完成会议音频，文本永不删）
            {
                let db = resume_db.clone();
                let ah = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(300)).await;
                        let db2 = db.clone();
                        commands::consistency_check_and_repair(&db2, &ah).await;
                        commands::enforce_disk_watermark(&db2).await;
                        commands::resume_unfinished_transcriptions(db2, &ah).await;
                    }
                });
            }

            // v2.5.1 每日自动备份：db + 近 7 天音频（9-24 三场会议音频永久丢失的防线）
            // 策略：运行期间每小时检查"今日是否已备份"，未备则执行——不定点开机也能保住当天数据
            {
                let ah = app_handle.clone();
                tauri::async_runtime::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(300)).await; // 启动 5 分钟后首跑（避开启动高峰）
                    loop {
                        commands::run_auto_backup_if_needed(&ah).await;
                        tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                    }
                });
            }

            Ok(())
        })
        .plugin(tauri_plugin_shell::init())
        .invoke_handler(tauri::generate_handler![
            commands::health_check,
            commands::create_chat_session,
            commands::list_chat_sessions,
            commands::get_chat_session,
            commands::update_chat_title,
            commands::send_message,
            commands::stream_message,
            commands::rate_message,
            commands::delete_chat_session,
            commands::distill_evolution,
            commands::get_evolution_insights,
            commands::create_note,
            commands::list_notes,
            commands::get_note,
            commands::update_note,
            commands::delete_note,
            commands::search,
            commands::create_meeting,
            commands::list_meetings,
            commands::list_meetings_by_note,
            commands::sync_note_title_to_meetings,
            commands::get_meeting,
            commands::update_meeting_title,
            commands::auto_meeting_title,
            commands::get_meeting_audio,
            commands::delete_meeting,
            commands::upload_audio,
            commands::trigger_aggregate,
            commands::trigger_correction,
            commands::get_summary,
            commands::get_transcript,
            commands::edit_transcript_chunk,
            commands::asr_status,
            commands::asr_ensure_running,
            commands::asr_launchd_status,
            commands::asr_install_launchd,
            commands::asr_uninstall_launchd,
            commands::start_recording,
            commands::stop_recording,
            commands::get_recording_status,
            commands::run_speaker_diarization,
            commands::run_cross_validate_transcribe,
            commands::rename_speaker,
            // P5 新增命令
            commands::enroll_speaker,
            commands::list_voiceprints,
            commands::delete_voiceprint,
            commands::add_hotword,
            commands::list_hotwords,
            commands::delete_hotword,
            commands::import_hotwords,
            commands::get_correction_stats,
            // 截图
            commands::capture_screenshot,
            // 许可证管理
            commands::activate_license,
            commands::get_license_status,
            commands::deactivate_license,
            // 功能门控查询
            commands::check_feature,
            // 应用设置
            commands::get_setting,
            commands::set_setting,
             commands::list_settings,
             // 数据备份与恢复
             commands::backup_database,
             commands::restore_database,
             commands::list_backups,
             // v2.5.2 转写状态卡片 + 失败段重试
             commands::get_transcription_stats,
             commands::retry_failed_segments,
             // v2.5.2 纪要导出
             commands::export_summary,
             // v2.6.0 纠错反哺闭环 + 健康报告
             commands::get_correction_hotword_suggestions,
             commands::apply_correction_hotwords,
             commands::get_health_report,
         ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// 笔尖APP - tauri::command

use crate::models::*;
use crate::state::AppState;
use tauri::{AppHandle, Emitter, Manager, State};
use uuid::Uuid;
use sqlx::Row;

// ========== 录音分段常量（P8: 30s→10s，提升准实时出字速度）==========
const SEG_DURATION_SEC: f64 = 10.0;       // 单段录音时长（秒）
const SEG_DURATION_STR: &str = "10";      // ffmpeg -segment_time 参数

// ========== 健康检查 ==========

#[tauri::command]
pub async fn health_check(_state: State<'_, AppState>) -> Result<HealthResponse, String> {
    // 实际探测 Ollama 和 ASR 服务
    let ollama_status = check_service_health("http://127.0.0.1:11434/api/tags", 2).await;
    let asr_primary = check_service_health("http://127.0.0.1:8083/health", 2).await;
    let asr_secondary = check_service_health("http://127.0.0.1:8082/health", 2).await;
    let asr_status = if asr_primary == "ok" || asr_secondary == "ok" { "ok" } else { "offline" };
    Ok(HealthResponse {
        status: "ok".to_string(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        services: serde_json::json!({
            "server": "ok",
            "database": "ok",
            "ollama": ollama_status,
            "asr": asr_status
        }),
    })
}

/// 探测 HTTP 端点是否可达，返回 "ok" 或 "offline"
async fn check_service_health(url: &str, timeout_secs: u64) -> &'static str {
    match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build()
    {
        Ok(client) => {
            match client.get(url).send().await {
                Ok(resp) if resp.status().is_success() => "ok",
                _ => "offline",
            }
        }
        Err(_) => "offline",
    }
}

// ========== v2.6.0 健康报告 + 磁盘水位管理 ==========
// 不翻日志即可判断今天转写是否正常；audio_cache 超 90% 水位自动归档最旧会议音频（文本永不删）。

/// 磁盘占用（%）：df 解析（macOS 自带，避免引入 libc/statvfs 绑定）
fn disk_used_percent(path: &str) -> Option<f64> {
    let out = std::process::Command::new("df").args(["-k", path]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().nth(1)?;
    let cols: Vec<&str> = line.split_whitespace().collect();
    if cols.len() >= 3 {
        let total: f64 = cols[1].parse().ok()?;
        let used: f64 = cols[2].parse().ok()?;
        if total > 0.0 { Some(used / total * 100.0) } else { None }
    } else { None }
}

fn dir_size_bytes(p: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(md) = std::fs::metadata(p) {
        if md.is_file() { return md.len(); }
    }
    if let Ok(entries) = std::fs::read_dir(p) {
        for e in entries.flatten() {
            total += dir_size_bytes(&e.path());
        }
    }
    total
}

/// audio_cache 三套目录（目录统一前兼容）总大小（MB）
pub fn audio_cache_total_mb() -> f64 {
    all_audio_cache_dirs().iter().map(|d| dir_size_bytes(d)).sum::<u64>() as f64 / 1024.0 / 1024.0
}

/// 磁盘水位执行：>90% 时按最旧优先归档已完成会议的音频目录（zip 至 backups/archived_audio/），
/// 原文本记录（chunks/final_summaries）永不删。归档后释放原目录。
pub async fn enforce_disk_watermark(db: &crate::db::Database) {
    let home = std::env::var("HOME").unwrap_or_default();
    let Some(used_pct) = disk_used_percent(&home) else { return };
    if used_pct < 90.0 {
        crate::logger::log("DiskWatermark", &format!("磁盘占用 {:.1}%（水位线 90% 以下，无需归档）", used_pct));
        return;
    }
    crate::logger::log("DiskWatermark", &format!("磁盘占用 {:.1}% 超水位线，开始归档最旧已完成会议音频", used_pct));
    // 找最旧的已终态、有 session_dir 的会议（completed/transcribed/transcription_partial/no_voice/failed，
    // 不含 recording——录音中的绝不动）
    let rows: Vec<(String, String)> = sqlx::query(
        "SELECT id, session_dir FROM meetings WHERE session_dir != '' AND status IN ('completed','transcribed','transcription_partial','no_voice','failed') ORDER BY created_at ASC LIMIT 3"
    )
    .fetch_all(db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<String,_>("id"), r.get::<String,_>("session_dir"))).collect())
    .unwrap_or_default();
    if rows.is_empty() {
        crate::logger::log("DiskWatermark", "无可归档的已完成会议（录音中或无目录），跳过");
        return;
    }
    let backup_root = format!("{}/Library/Application Support/com.bijian.app/backups/archived_audio", home);
    let _ = tokio::fs::create_dir_all(&backup_root).await;
    for (mid, sd) in &rows {
        let src = std::path::Path::new(sd);
        if !src.exists() { continue; }
        let name = src.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| mid.clone());
        let dst = format!("{}/{}.zip", backup_root, name);
        // zip 归档（macOS 自带 /usr/bin/zip）；-q 静默，-r 递归
        let out = std::process::Command::new("zip").args(["-q", "-r", &dst, sd]).output();
        match out {
            Ok(o) if o.status.success() => {
                // 归档成功后删除原音频目录（zip 已在备份区保底）
                if tokio::fs::remove_dir_all(src).await.is_ok() {
                    let short: String = mid.chars().take(8).collect();
                    crate::logger::log("DiskWatermark", &format!("会议 {} 音频已归档 {}（文本记录保留）", short, dst));
                }
            }
            _ => crate::logger::log("DiskWatermark", &format!("归档失败（zip 异常），保留原目录 {}（宁可占盘不可丢数据）", sd)),
        }
    }
    let after = disk_used_percent(&home).unwrap_or(0.0);
    crate::logger::log("DiskWatermark", &format!("水位处理完成：{:.1}% → {:.1}%", used_pct, after));
}

/// 健康报告（诊断面板数据源）：今日/累计转写指标 + 磁盘 + 备份 + 服务状态
#[tauri::command]
pub async fn get_health_report(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    // 今日段级指标
    let t = sqlx::query(
        "SELECT COUNT(*) as total, COALESCE(SUM(CASE WHEN state='done' THEN 1 ELSE 0 END),0) as done, COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0) as failed, COALESCE(SUM(CASE WHEN state='silent' THEN 1 ELSE 0 END),0) as silent, COALESCE(SUM(CASE WHEN engine LIKE 'fallback%' THEN 1 ELSE 0 END),0) as fallback FROM transcription_segments WHERE finished_at LIKE ?"
    )
    .bind(format!("{}%", today))
    .fetch_one(state.db.pool()).await.map_err(|e| e.to_string())?;
    // 累计
    let a = sqlx::query(
        "SELECT COUNT(*) as total, COALESCE(SUM(CASE WHEN state='done' THEN 1 ELSE 0 END),0) as done, COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0) as failed FROM transcription_segments"
    )
    .fetch_one(state.db.pool()).await.map_err(|e| e.to_string())?;
    let m = sqlx::query(
        "SELECT COUNT(*) as total, COALESCE(SUM(CASE WHEN status IN ('transcribed','completed') THEN 1 ELSE 0 END),0) as ok, COALESCE(SUM(CASE WHEN status='no_voice' THEN 1 ELSE 0 END),0) as nv FROM meetings"
    )
    .fetch_one(state.db.pool()).await.map_err(|e| e.to_string())?;
    // 备份状态
    let last_backup: Option<String> = sqlx::query("SELECT value FROM settings WHERE key = 'last_auto_backup_date'")
        .fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?
        .map(|r| r.get("value"));
    // 服务
    let ollama = check_service_health("http://127.0.0.1:11434/api/tags", 2).await;
    let asr = check_service_health("http://127.0.0.1:8083/health", 2).await;
    let home = std::env::var("HOME").unwrap_or_default();
    let used_pct = disk_used_percent(&home);
    Ok(serde_json::json!({
        "today": {
            "segments": t.get::<i64,_>("total"), "done": t.get::<i64,_>("done"),
            "failed": t.get::<i64,_>("failed"), "silent": t.get::<i64,_>("silent"),
            "fallback": t.get::<i64,_>("fallback"),
        },
        "all_time": {
            "segments": a.get::<i64,_>("total"), "done": a.get::<i64,_>("done"), "failed": a.get::<i64,_>("failed"),
            "meetings": m.get::<i64,_>("total"), "meetings_ok": m.get::<i64,_>("ok"), "meetings_no_voice": m.get::<i64,_>("nv"),
        },
        "disk": {
            "audio_cache_mb": (audio_cache_total_mb() * 10.0).round() / 10.0,
            "used_percent": used_pct.map(|v| (v * 10.0).round() / 10.0),
            "watermark": 90,
        },
        "backup": { "last_auto_backup_date": last_backup },
        "services": { "ollama": ollama, "asr_8083": asr },
    }))
}

// ========== 角色提示词 ==========

fn build_system_prompt(role: &str) -> String {
    match role {
        "proofread" => "你是一个专业的校对助手。请检查用户输入的文本，指出错别字、语法错误、标点问题，并给出修改建议。\n输出格式用表格：| 问题类型 | 原文 | 建议 | 严重度 |\n只标问题不直接改原文。".to_string(),
        "polish" => "你是一个专业的文字润色助手。请优化用户输入的文本，使其更流畅、更书面化，同时保留原意。输出润色后的全文，不要解释改动。".to_string(),
        "expand" => "你是一个专业的扩写助手。请根据用户输入的主题或段落，进行合理的扩写和延伸，保持原有风格和语调。".to_string(),
        "summarize" => "你是一个专业的缩写助手。请精简用户输入的文本，提取核心要点，使其更简洁。保留关键数据和结论。".to_string(),
        _ => "你是PanNote，一个运行在用户本地的 AI 助手。请用中文回答，保持简洁清晰。\n当有搜索结果时，必须基于搜索结果回答，不要忽略搜索结果自行编造。".to_string(),
    }
}

// ========== AI 对话 ==========

#[tauri::command]
pub async fn create_chat_session(state: State<'_, AppState>, title: Option<String>, role: Option<String>) -> Result<ChatSession, String> {
    let id = Uuid::new_v4().to_string();
    let title = title.unwrap_or_default();
    let role = role.unwrap_or_else(|| "chat".to_string());
    sqlx::query("INSERT INTO sessions (id, title, role) VALUES (?, ?, ?)").bind(&id).bind(&title).bind(&role).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(ChatSession { id, title, role, created_at: chrono::Utc::now().to_rfc3339(), updated_at: chrono::Utc::now().to_rfc3339(), message_count: 0 })
}

#[tauri::command]
pub async fn list_chat_sessions(state: State<'_, AppState>) -> Result<Vec<ChatSession>, String> {
    let rows = sqlx::query("SELECT id, title, role, created_at, updated_at FROM sessions ORDER BY updated_at DESC").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| ChatSession { id: r.get("id"), title: r.get("title"), role: r.get("role"), created_at: r.get("created_at"), updated_at: r.get("updated_at"), message_count: 0 }).collect())
}

#[tauri::command]
pub async fn get_chat_session(state: State<'_, AppState>, session_id: String) -> Result<serde_json::Value, String> {
    let session = sqlx::query("SELECT id, title, role, created_at, updated_at FROM sessions WHERE id = ?").bind(&session_id).fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
    let messages = sqlx::query("SELECT id, role, content, rating, ts FROM messages WHERE session_id = ? ORDER BY ts ASC").bind(&session_id).fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    let messages_json: Vec<serde_json::Value> = messages.iter().map(|m| serde_json::json!({"id": m.get::<String,_>("id"), "role": m.get::<String,_>("role"), "content": m.get::<String,_>("content"), "rating": m.get::<i64,_>("rating") as i32, "ts": m.get::<String,_>("ts")})).collect();
    if let Some(row) = session {
        Ok(serde_json::json!({"session": {"id": row.get::<String,_>("id"), "title": row.get::<String,_>("title"), "role": row.get::<String,_>("role"), "created_at": row.get::<String,_>("created_at"), "updated_at": row.get::<String,_>("updated_at")}, "messages": messages_json}))
    } else { Err("会话不存在".to_string()) }
}

#[tauri::command]
pub async fn update_chat_title(state: State<'_, AppState>, session_id: String, title: String) -> Result<(), String> {
    sqlx::query("UPDATE sessions SET title = ? WHERE id = ?").bind(&title).bind(&session_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn send_message(state: State<'_, AppState>, session_id: String, message: String, _role: Option<String>, _model: Option<String>, _enable_tools: Option<bool>) -> Result<SendMessageResponse, String> {
    let msg_id = Uuid::new_v4().to_string();
    let ts = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO messages (id, session_id, role, content, ts) VALUES (?, ?, 'user', ?, ?)").bind(&msg_id).bind(&session_id).bind(&message).bind(&ts).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?").bind(&now).bind(&session_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(SendMessageResponse { message_id: msg_id, session_id })
}

#[tauri::command]
pub async fn stream_message(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    session_id: String,
    message: String,
    model: Option<String>,
    enable_tools: Option<bool>,
) -> Result<serde_json::Value, String> {
    let model = model.unwrap_or_else(|| "qwen3-4b-32k".to_string());
    let enable_tools = enable_tools.unwrap_or(false);

    // 转写/纪要进行中时拦截 4B 调用：CPU 互相抢占会卡死，且 ASR 优先级更高
    let running_jobs: i64 = match sqlx::query("SELECT COUNT(*) AS c FROM jobs WHERE status = 'processing'")
        .fetch_optional(state.db.pool()).await {
        Ok(Some(r)) => r.get::<i64, _>("c"),
        _ => 0,
    };
    if running_jobs > 0 {
        let _ = app_handle.emit(
            &format!("chat_stream_{}", session_id),
            serde_json::json!({"error": "录音转写/纪要生成进行中，4B 模型暂不可用，请等待转写完成后再对话", "done": true}),
        );
        return Ok(serde_json::json!({"streaming": false, "blocked": true, "reason": "asr_running"}));
    }
    let ollama_url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    state.ollama_manager.touch();

    // 获取会话角色
    let role = sqlx::query("SELECT role FROM sessions WHERE id = ?")
        .bind(&session_id)
        .fetch_optional(state.db.pool()).await
        .map_err(|e| e.to_string())?
        .map(|r| r.get::<String, _>("role"))
        .unwrap_or_else(|| "chat".to_string());

    let system_prompt = build_system_prompt(&role);

    // 保存用户消息
    let user_msg_id = Uuid::new_v4().to_string();
    let ts = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO messages (id, session_id, role, content, ts) VALUES (?, ?, 'user', ?, ?)")
        .bind(&user_msg_id).bind(&session_id).bind(&message).bind(&ts)
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;

    // 更新会话时间
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE sessions SET updated_at = ? WHERE id = ?")
        .bind(&now).bind(&session_id)
        .execute(state.db.pool()).await.ok();

    // qwen3 默认开 thinking → 工具调用推理极慢且二轮 content 为空。
    // 给用户消息加 /no_think 前缀禁用思考（qwen3 原生支持）。
    // 历史消息本身可能已带前缀（首次注入），不应重复添加。
    let is_single_turn = matches!(role.as_str(), "proofread" | "polish" | "expand" | "summarize");

    let mut msgs: Vec<serde_json::Value> = if is_single_turn {
        // 单回合任务：只传 system + 用户当前消息，避免历史膨胀拖慢 prompt 处理
        vec![serde_json::json!({"role": "user", "content": format!("/no_think {}", message)})]
    } else {
        // 对话模式：取最近 6 条（3 轮）历史，控制 prompt 长度
        let history = sqlx::query("SELECT role, content FROM messages WHERE session_id = ? ORDER BY ts ASC")
            .bind(&session_id)
            .fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
        history.iter().rev().take(7).rev().map(|h| {
            let content: String = h.get("content");
            // 历史的 user 消息确保带 /no_think（旧消息可能没带）
            let role: String = h.get("role");
            if role == "user" && !content.starts_with("/no_think") {
                serde_json::json!({"role": "user", "content": format!("/no_think {}", content)})
            } else {
                serde_json::json!({"role": role, "content": content})
            }
        }).collect()
    };
    msgs.insert(0, serde_json::json!({"role": "system", "content": system_prompt}));

    // 构建 Ollama 请求
    let mut payload = serde_json::json!({
        "model": model,
        "messages": msgs,
        "stream": true,
        "options": {"num_ctx": 8192, "num_predict": 2048}
    });

    if enable_tools {
        payload["tools"] = serde_json::json!([{
            "type": "function",
            "function": {
                "name": "web_search",
                "description": "搜索互联网获取最新信息。当用户询问最新消息、天气、股价、汇率等需要联网的内容时使用。",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "搜索关键词"}
                    },
                    "required": ["query"]
                }
            }
        }]);
    }

    // 克隆资源给后台任务
    let db = state.db.clone();
    let http_client = state.http_client.clone();
    let sid = session_id.clone();
    let event_name = format!("chat_stream_{}", session_id);

    // —— 负载检测：检查 ASR 是否在跑 + 系统 loadavg ——
    let check_url = ollama_url.clone();
    let check_client = http_client.clone();
    let check_event = event_name.clone();
    let check_handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        // 1) 查 ASR 进程是否存活
        let asr_running = tokio::process::Command::new("pgrep")
            .args(["-f", "sherpa_asr_server"])
            .output()
            .await
            .map(|o| !o.stdout.is_empty())
            .unwrap_or(false);

        // 2) 查系统 loadavg (macOS: sysctl -n vm.loadavg)
        let loadavg = tokio::process::Command::new("sysctl")
            .args(["-n", "vm.loadavg"])
            .output()
            .await
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .and_then(|s| {
                // 输出形如 "{ 2.31 3.42 5.10 }"，取 1 分钟负载
                let parts: Vec<&str> = s.trim().split_whitespace().collect();
                parts.get(1).and_then(|p| p.parse::<f64>().ok())
            })
            .unwrap_or(0.0);

        // 3) 查 Ollama 是否有活跃 slot 在跑（排除当前请求自身）
        let ollama_busy = match check_client
            .get(format!("{}/api/ps", check_url))
            .timeout(std::time::Duration::from_secs(3))
            .send().await
        {
            Ok(r) => match r.text().await {
                Ok(body) => serde_json::from_str::<serde_json::Value>(&body)
                    .ok()
                    .and_then(|v| {
                        v.get("models")
                            .and_then(|m| m.as_array())
                            .map(|arr| !arr.is_empty())
                    })
                    .unwrap_or(false),
                Err(_) => false,
            },
            Err(_) => false,
        };

        // 判断条件：ASR 在跑 且 loadavg > 6（6 核机器满载线）
        if asr_running && loadavg > 6.0 {
            let est_wait = if ollama_busy {
                "60-120".to_string()
            } else {
                "30-60".to_string()
            };
            let _ = check_handle.emit(&check_event, serde_json::json!({
                "type": "load_warning",
                "message": format!(
                    "⚠️ 系统资源紧张\nASR 转写服务正在运行，CPU 负载 {:.1}（正常≤6）\n预计等待 {} 秒，请耐心等待",
                    loadavg, est_wait
                ),
                "loadavg": loadavg,
                "asr_running": true,
                "est_wait_sec": est_wait,
            }));
        }
    });

    // 启动后台流式任务
    tauri::async_runtime::spawn(async move {
        // 流式请求：用 per-request timeout（600s），不依赖全局 client timeout
        let resp = match http_client.post(format!("{}/api/chat", ollama_url))
            .timeout(std::time::Duration::from_secs(600))
            .json(&payload)
            .send().await {
            Ok(r) => r,
            Err(e) => {
                let _ = app_handle.emit(&event_name, serde_json::json!({
                    "error": format!("无法连接 Ollama: {}", e),
                    "done": true
                }));
                return;
            }
        };

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            let _ = app_handle.emit(&event_name, serde_json::json!({
                "error": format!("Ollama 返回错误 {}: {}", status, body),
                "done": true
            }));
            return;
        }

        let mut full_content = String::new();
        let mut buffer = String::new();
        let mut tool_results: Vec<serde_json::Value> = Vec::new();
        let mut tool_call_meta: Vec<(String, String, serde_json::Value)> = Vec::new(); // (id, name, args)
        let mut has_tool_calls = false;

        let mut resp = resp;
        loop {
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,  // 流结束
                Err(e) => {
                    // 超时或网络中断 → 通知前端报错，不再静默吞掉
                    let _ = app_handle.emit(&event_name, serde_json::json!({
                        "error": format!("连接中断: {}", e),
                        "done": true
                    }));
                    // 保存已生成部分
                    if !full_content.is_empty() {
                        let ai_id = Uuid::new_v4().to_string();
                        let ts = chrono::Utc::now().to_rfc3339();
                        let _ = sqlx::query("INSERT INTO messages (id, session_id, role, content, ts) VALUES (?, ?, 'assistant', ?, ?)")
                            .bind(&ai_id).bind(&sid).bind(&full_content).bind(&ts)
                            .execute(db.pool()).await;
                    }
                    return;
                }
            };
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            // 处理完整行 (NDJSON)
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();
                if line.is_empty() { continue; }

                let obj: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(o) => o,
                    Err(_) => continue,
                };

                let msg = obj.get("message").unwrap_or(&serde_json::Value::Null);

                // 检查工具调用
                if let Some(tool_calls) = msg.get("tool_calls").and_then(|t| t.as_array()) {
                    if !tool_calls.is_empty() {
                        has_tool_calls = true;
                        for tc in tool_calls {
                            let func = tc.get("function").unwrap_or(&serde_json::Value::Null);
                            let name = func.get("name").and_then(|n| n.as_str()).unwrap_or("");
                            let args = func.get("arguments").cloned().unwrap_or(serde_json::json!({}));
                            let tc_id = tc.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string();

                            let _ = app_handle.emit(&event_name, serde_json::json!({
                                "type": "tool_call",
                                "tool": name,
                                "arguments": args
                            }));

                            if name == "web_search" {
                                if !crate::feature::is_feature_enabled(crate::feature::FeatureFlag::WebSearch) {
                                    let _ = app_handle.emit(&event_name, serde_json::json!({
                                        "type": "tool_error",
                                        "tool": name,
                                        "message": "联网搜索功能需要 Pro 版"
                                    }));
                                    break;
                                }
                                let query = args.get("query").and_then(|q| q.as_str()).unwrap_or("");
                                let _ = app_handle.emit(&event_name, serde_json::json!({
                                    "type": "tool_call",
                                    "tool": name,
                                    "query": query
                                }));
                                let results = crate::services::search::web_search(query).await.unwrap_or_default();

                                // 检查是否搜索失败（返回了 error 类型结果）
                                let is_error = results.iter().any(|r| {
                                    r.get("source").and_then(|s| s.as_str()) == Some("error")
                                });

                                if is_error {
                                    let _ = app_handle.emit(&event_name, serde_json::json!({
                                        "type": "tool_error",
                                        "tool": name,
                                        "message": "搜索服务不可用，请检查网络连接"
                                    }));
                                } else {
                                    // 把搜索结果摘要推给前端展示
                                    let snippets: Vec<serde_json::Value> = results.iter().take(3).map(|r| {
                                        serde_json::json!({
                                            "title": r.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                                            "snippet": r.get("snippet").and_then(|s| s.as_str()).unwrap_or(""),
                                        })
                                    }).collect();
                                    let _ = app_handle.emit(&event_name, serde_json::json!({
                                        "type": "tool_result",
                                        "tool": name,
                                        "count": results.len(),
                                        "previews": snippets
                                    }));
                                }

                                // 把搜索结果格式化为 4B 模型能读懂的自然语言
                                let search_context = results.iter().take(5).enumerate().map(|(i, r)| {
                                    let title = r.get("title").and_then(|t| t.as_str()).unwrap_or("");
                                    let snippet = r.get("snippet").and_then(|s| s.as_str()).unwrap_or("");
                                    format!("{}. {}\n   {}", i + 1, title, snippet)
                                }).collect::<Vec<_>>().join("\n");
                                let tool_content = format!("以下是搜索到的相关信息：\n{}\n\n请基于以上信息回答用户的问题。如果以上信息不足以回答，请说明并补充你的知识。", search_context);

                                tool_call_meta.push((tc_id, name.to_string(), args.clone()));
                                tool_results.push(serde_json::json!({
                                    "role": "tool",
                                    "content": tool_content
                                }));
                            }
                        }
                    }
                }

                // 文本内容
                if let Some(delta) = msg.get("content").and_then(|c| c.as_str()) {
                    if !delta.is_empty() {
                        full_content.push_str(delta);
                        let _ = app_handle.emit(&event_name, serde_json::json!({
                            "delta": delta
                        }));
                    }
                }

                if obj.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                    break;
                }
            }
        }

        // 如果有工具调用，带结果再调一次 Ollama
        // 注意：assistant 消息必须携带 tool_calls 字段，tool 消息必须携带 tool_call_id，
        // 否则 qwen3 无法正确关联工具结果（OpenAI function calling 协议要求）。
        if has_tool_calls && !tool_results.is_empty() {
            // 构造 assistant 消息（含 tool_calls）
            let assistant_tool_calls: Vec<serde_json::Value> = tool_call_meta.iter().map(|(tc_id, name, args)| {
                serde_json::json!({
                    "id": tc_id,
                    "type": "function",
                    "function": {"name": name, "arguments": args}
                })
            }).collect();
            msgs.push(serde_json::json!({
                "role": "assistant",
                "content": full_content.clone(),
                "tool_calls": assistant_tool_calls
            }));
            for (i, tr) in tool_results.iter().enumerate() {
                let tc_id = tool_call_meta.get(i).map(|(id, _, _)| id.clone()).unwrap_or_default();
                let mut tr_msg = tr.clone();
                tr_msg["tool_call_id"] = serde_json::json!(tc_id);
                msgs.push(tr_msg);
            }

            let payload2 = serde_json::json!({
                "model": model,
                "messages": msgs,
                "stream": true,
                "options": {"num_ctx": 8192, "num_predict": 2048}
            });

            let resp2 = match http_client.post(format!("{}/api/chat", ollama_url))
                .timeout(std::time::Duration::from_secs(600))
                .json(&payload2)
                .send().await {
                Ok(r) => r,
                Err(e) => {
                    let _ = app_handle.emit(&event_name, serde_json::json!({
                        "error": format!("工具调用后再次请求失败: {}", e),
                        "done": true
                    }));
                    return;
                }
            };

            let mut resp2 = resp2;
            let mut buffer2 = String::new();
            // 二轮清空——一轮的文本是模型在没有搜索结果时的瞎猜，必须丢弃
            full_content.clear();
            // 通知前端也清空已显示的一轮文本
            let _ = app_handle.emit(&event_name, serde_json::json!({
                "type": "tool_round_done"
            }));

            loop {
                let chunk = match resp2.chunk().await {
                    Ok(Some(c)) => c,
                    Ok(None) => break,
                    Err(e) => {
                        let _ = app_handle.emit(&event_name, serde_json::json!({
                            "error": format!("连接中断: {}", e),
                            "done": true
                        }));
                        if !full_content.is_empty() {
                            let ai_id = Uuid::new_v4().to_string();
                            let ts = chrono::Utc::now().to_rfc3339();
                            let _ = sqlx::query("INSERT INTO messages (id, session_id, role, content, ts) VALUES (?, ?, 'assistant', ?, ?)")
                                .bind(&ai_id).bind(&sid).bind(&full_content).bind(&ts)
                                .execute(db.pool()).await;
                        }
                        return;
                    }
                };
                buffer2.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(pos) = buffer2.find('\n') {
                    let line = buffer2[..pos].trim().to_string();
                    buffer2 = buffer2[pos + 1..].to_string();
                    if line.is_empty() { continue; }
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(&line) {
                        if let Some(delta) = obj.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                            if !delta.is_empty() {
                                full_content.push_str(delta);
                                let _ = app_handle.emit(&event_name, serde_json::json!({"delta": delta}));
                            }
                        }
                        if obj.get("done").and_then(|d| d.as_bool()).unwrap_or(false) {
                            break;
                        }
                    }
                }
            }
        }

        // 保存 AI 回复
        let ai_id = Uuid::new_v4().to_string();
        let ts = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query("INSERT INTO messages (id, session_id, role, content, ts) VALUES (?, ?, 'assistant', ?, ?)")
            .bind(&ai_id).bind(&sid).bind(&full_content).bind(&ts)
            .execute(db.pool()).await;

        // 通知前端完成
        let _ = app_handle.emit(&event_name, serde_json::json!({
            "done": true,
            "message_id": ai_id
        }));
    });

    Ok(serde_json::json!({"streaming": true, "session_id": session_id}))
}

#[tauri::command]
pub async fn rate_message(state: State<'_, AppState>, message_id: String, rating: i32) -> Result<(), String> {
    sqlx::query("UPDATE messages SET rating = ? WHERE id = ?").bind(rating as i64).bind(&message_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_chat_session(state: State<'_, AppState>, session_id: String) -> Result<(), String> {
    sqlx::query("DELETE FROM messages WHERE session_id = ?").bind(&session_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM sessions WHERE id = ?").bind(&session_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(())
}

// ========== 进化系统 ==========

#[tauri::command]
pub async fn distill_evolution(state: State<'_, AppState>) -> Result<String, String> {
    let rows = sqlx::query("SELECT content FROM messages WHERE rating = 1 ORDER BY ts DESC LIMIT 10").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    let count = rows.len();
    if count == 0 { return Ok("暂无足够的高评分消息".to_string()); }
    let id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO evolution_insights (id, insight_type, content, confidence) VALUES (?, 'user_preference', ?, 0.7)").bind(&id).bind(format!("从 {} 条高评分消息中提取", count)).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(format!("蒸馏完成，处理了 {} 条消息", count))
}

#[tauri::command]
pub async fn get_evolution_insights(state: State<'_, AppState>) -> Result<Vec<EvolutionInsight>, String> {
    let rows = sqlx::query("SELECT id, insight_type, content, confidence, created_at FROM evolution_insights ORDER BY created_at DESC LIMIT 20").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| EvolutionInsight { id: r.get("id"), insight_type: r.get("insight_type"), content: r.get("content"), confidence: r.get::<f64,_>("confidence"), created_at: r.get("created_at") }).collect())
}

// ========== 笔记管理 ==========

#[tauri::command]
pub async fn create_note(state: State<'_, AppState>, title: String, content: String, tags: Option<String>) -> Result<Note, String> {
    let id = Uuid::new_v4().to_string();
    let tags = tags.unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO notes (id, title, content, tags, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)").bind(&id).bind(&title).bind(&content).bind(&tags).bind(&now).bind(&now).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(Note { id, title, content, tags, created_at: now.clone(), updated_at: now })
}

#[tauri::command]
pub async fn list_notes(state: State<'_, AppState>) -> Result<Vec<Note>, String> {
    let rows = sqlx::query("SELECT id, title, content, tags, created_at, updated_at FROM notes ORDER BY updated_at DESC").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| Note { id: r.get("id"), title: r.get("title"), content: r.get("content"), tags: r.get("tags"), created_at: r.get("created_at"), updated_at: r.get("updated_at") }).collect())
}

#[tauri::command]
pub async fn get_note(state: State<'_, AppState>, note_id: String) -> Result<Option<Note>, String> {
    let row = sqlx::query("SELECT id, title, content, tags, created_at, updated_at FROM notes WHERE id = ?").bind(&note_id).fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(row.map(|r| Note { id: r.get("id"), title: r.get("title"), content: r.get("content"), tags: r.get("tags"), created_at: r.get("created_at"), updated_at: r.get("updated_at") }))
}

#[tauri::command]
pub async fn update_note(state: State<'_, AppState>, note_id: String, title: Option<String>, content: Option<String>, tags: Option<String>) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let mut qb = sqlx::QueryBuilder::new("UPDATE notes SET updated_at = ");
    qb.push_bind(now);
    if let Some(t) = &title { qb.push(", title = "); qb.push_bind(t); }
    if let Some(c) = &content { qb.push(", content = "); qb.push_bind(c); }
    if let Some(t) = &tags { qb.push(", tags = "); qb.push_bind(t); }
    qb.push(" WHERE id = ");
    qb.push_bind(note_id);
    qb.build().execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_note(state: State<'_, AppState>, note_id: String) -> Result<(), String> {
    let mut tx = state.db.pool().begin().await.map_err(|e| format!("开启事务失败: {}", e))?;
    // v2.4: 挂载在本笔记下的会话置空 note_id → 变回独立录音条目，零丢失
    sqlx::query("UPDATE meetings SET note_id = '' WHERE note_id = ?").bind(&note_id).execute(&mut *tx).await.map_err(|e| format!("解挂会话失败: {}", e))?;
    sqlx::query("DELETE FROM notes WHERE id = ?").bind(&note_id).execute(&mut *tx).await.map_err(|e| format!("删笔记失败: {}", e))?;
    tx.commit().await.map_err(|e| format!("提交事务失败: {}", e))?;
    Ok(())
}

// ========== 搜索 ==========

#[tauri::command]
pub async fn search(state: State<'_, AppState>, query: String, limit: Option<i64>) -> Result<Vec<SearchResult>, String> {
    let limit = limit.unwrap_or(20);
    let rows = sqlx::query("SELECT 'note' as source, f.title, substr(f.content, 1, 200) as snippet FROM notes_fts f WHERE notes_fts MATCH ? LIMIT ?").bind(&query).bind(limit).fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| SearchResult { source: r.get("source"), title: r.get("title"), snippet: r.get("snippet"), url: None }).collect())
}

// ========== 会议管理 ==========

/// 创建会话（可选挂载到笔记：note_id 非空时为"笔记会话"，列表不单独展示）
#[tauri::command]
pub async fn create_meeting(state: State<'_, AppState>, title: Option<String>, note_id: Option<String>) -> Result<Meeting, String> {
    let id = Uuid::new_v4().to_string();
    let title = title.unwrap_or_default();
    let note_id = note_id.unwrap_or_default();
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("INSERT INTO meetings (id, title, status, created_at, updated_at, note_id) VALUES (?, ?, 'pending', ?, ?, ?)").bind(&id).bind(&title).bind(&now).bind(&now).bind(&note_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(Meeting { id, title, status: "pending".to_string(), duration: 0, created_at: now.clone(), updated_at: now, session_dir: String::new(), note_id })
}

#[tauri::command]
pub async fn list_meetings(state: State<'_, AppState>) -> Result<Vec<Meeting>, String> {
    let rows = sqlx::query("SELECT id, title, status, duration, created_at, updated_at, session_dir, COALESCE(note_id, '') as note_id FROM meetings ORDER BY created_at DESC").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| Meeting { id: r.get("id"), title: r.get("title"), status: r.get("status"), duration: r.get::<i64,_>("duration"), created_at: r.get("created_at"), updated_at: r.get("updated_at"), session_dir: r.get::<String,_>("session_dir"), note_id: r.try_get::<String,_>("note_id").unwrap_or_default() }).collect())
}

#[tauri::command]
pub async fn get_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<Option<Meeting>, String> {
    let row = sqlx::query("SELECT id, title, status, duration, created_at, updated_at, session_dir, COALESCE(note_id, '') as note_id FROM meetings WHERE id = ?").bind(&meeting_id).fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(row.map(|r| Meeting { id: r.get("id"), title: r.get("title"), status: r.get("status"), duration: r.get::<i64,_>("duration"), created_at: r.get("created_at"), updated_at: r.get("updated_at"), session_dir: r.try_get::<String,_>("session_dir").unwrap_or_default(), note_id: r.try_get::<String,_>("note_id").unwrap_or_default() }))
}

/// 手动修改会议标题
#[tauri::command]
pub async fn update_meeting_title(state: State<'_, AppState>, meeting_id: String, title: String) -> Result<serde_json::Value, String> {
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE id = ?")
        .bind(&title).bind(&now).bind(&meeting_id)
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"ok": true, "title": title}))
}

// ========== 笔记×录音合体 v2.4：会话归属 ==========

/// 列出某笔记下全部会话（录音档案），按创建时间倒序
#[tauri::command]
pub async fn list_meetings_by_note(state: State<'_, AppState>, note_id: String) -> Result<Vec<Meeting>, String> {
    let rows = sqlx::query("SELECT id, title, status, duration, created_at, updated_at, session_dir, COALESCE(note_id, '') as note_id FROM meetings WHERE note_id = ? ORDER BY created_at DESC")
        .bind(&note_id)
        .fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| Meeting { id: r.get("id"), title: r.get("title"), status: r.get("status"), duration: r.get::<i64,_>("duration"), created_at: r.get("created_at"), updated_at: r.get("updated_at"), session_dir: r.try_get::<String,_>("session_dir").unwrap_or_default(), note_id: r.try_get::<String,_>("note_id").unwrap_or_default() }).collect())
}

/// 笔记保存时同步标题到挂载会话（笔记标题为唯一标题源）
#[tauri::command]
pub async fn sync_note_title_to_meetings(state: State<'_, AppState>, note_id: String, title: String) -> Result<serde_json::Value, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let res = sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE note_id = ?")
        .bind(&title).bind(&now).bind(&note_id)
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"ok": true, "updated": res.rows_affected()}))
}

/// 获取会议录音存档路径（录音存档播放用）
/// 优先取 meetings.session_dir；为空则按创建时间在 audio_cache 目录近匹配 rec_ 目录
#[tauri::command]
pub async fn get_meeting_audio(state: State<'_, AppState>, meeting_id: String) -> Result<serde_json::Value, String> {
    let row = sqlx::query("SELECT session_dir, created_at FROM meetings WHERE id = ?")
        .bind(&meeting_id)
        .fetch_optional(state.db.pool()).await
        .map_err(|e| e.to_string())?;

    let (stored_dir, created_at) = match row {
        Some(r) => (r.try_get::<String, _>("session_dir").unwrap_or_default(), r.try_get::<String, _>("created_at").unwrap_or_default()),
        None => return Err("会议不存在".to_string()),
    };

    let temp_dir = format!("{}/Library/Application Support/笔尖/audio_cache", std::env::var("HOME").unwrap_or_default());

    // 找到目录后统一尝试返回 full_audio.wav（缺失则合并生成）
    let mut dir_candidates: Vec<String> = {
        let mut v = Vec::new();
        if !stored_dir.is_empty() {
            v.push(stored_dir.clone());
        }
        v
    };

    // 兜底：按创建时间近匹配（历史会议无 session_dir 时）
    // 注意：meetings.created_at 是 UTC ISO，目录名是本地时间戳（epoch 秒）
    // 两者差 8 小时（本地 UTC+8），需要换算
    if stored_dir.is_empty() {
        let created_local_ts = chrono::DateTime::parse_from_rfc3339(&created_at)
            .map(|dt| dt.with_timezone(&chrono::Local).timestamp())
            .unwrap_or(0);
        if created_local_ts > 0 {
            if let Ok(entries) = std::fs::read_dir(&temp_dir) {
                let mut best: Option<(i64, String)> = None; // (diff, path)
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let Some(ts_str) = name.strip_prefix("rec_") else { continue };
                    let Ok(ts) = ts_str.parse::<i64>() else { continue };
                    let diff = (ts - created_local_ts).abs();
                    if diff <= 600 && best.as_ref().map(|(bd, _)| diff < *bd).unwrap_or(true) {
                        best = Some((diff, entry.path().to_string_lossy().to_string()));
                    }
                }
                if let Some((_, dir)) = best {
                    dir_candidates.push(dir);
                }
            }
        }
    }

    // 遍历候选目录，返回 full_audio.wav（缺失则尝试合并生成）
    for dir in &dir_candidates {
        let full_audio = format!("{}/full_audio.wav", dir);
        if std::path::Path::new(&full_audio).exists() {
            return Ok(serde_json::json!({"audio_path": full_audio, "exists": true}));
        }
        // 有段文件但无合并文件 → 尝试合并生成
        let seg_000 = format!("{}/seg_000.wav", dir);
        if std::path::Path::new(&seg_000).exists() {
            match concat_wav_segments(dir) {
                Ok(path) => {
                    eprintln!("[Recording] 为会议 {} 合并生成录音存档: {}", meeting_id, path);
                    return Ok(serde_json::json!({"audio_path": path, "exists": true}));
                }
                Err(e) => {
                    eprintln!("[Recording] 合并录音存档失败: {}", e);
                    return Ok(serde_json::json!({"audio_path": seg_000, "exists": true, "partial": true}));
                }
            }
        }
    }

    Ok(serde_json::json!({"audio_path": "", "exists": false}))
}

/// 自动生成会议标题：取会议前若干段转写文本，用本地 Ollama 模型生成简短标题
#[tauri::command]
pub async fn auto_meeting_title(state: State<'_, AppState>, meeting_id: String) -> Result<serde_json::Value, String> {
    // 取前 12 段（约 2 分钟）转写文本作为标题素材
    let rows = sqlx::query("SELECT transcript FROM chunks WHERE meeting_id = ? ORDER BY start_time ASC LIMIT 12")
        .bind(&meeting_id)
        .fetch_all(state.db.pool()).await
        .map_err(|e| e.to_string())?;

    let snippet: String = rows.iter()
        .filter_map(|r| r.try_get::<String, _>("transcript").ok())
        .filter(|t| !t.trim().is_empty())
        .collect::<Vec<_>>().join("")
        .chars().take(300).collect();

    if snippet.trim().is_empty() {
        return Ok(serde_json::json!({"ok": false, "title": ""}));
    }

    let ollama_url = std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
    state.ollama_manager.touch();
    let model = "qwen3-4b-32k".to_string();

    let prompt = format!(
        "/no_think\n根据下面这段会议转写内容，生成一个简洁的会议标题（15字以内，不要引号，不要前缀）。\n\n转写内容：\n{}",
        snippet
    );

    let payload = serde_json::json!({
        "model": model,
        "messages": [{"role": "user", "content": prompt}],
        "stream": false,
        "options": {"num_ctx": 4096, "num_predict": 64}
    });

    let title = match state.http_client.post(format!("{}/api/chat", ollama_url))
        .json(&payload)
        .timeout(std::time::Duration::from_secs(60))
        .send().await
    {
        Ok(resp) => {
            let result: serde_json::Value = resp.json().await.unwrap_or_default();
            result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str())
                .unwrap_or("").trim().to_string()
        }
        Err(_) => String::new(),
    };

    let title = title.trim_matches(|c: char| c == '"' || c == '“' || c == '”' || c == ' ' || c == '\n').to_string();

    if !title.is_empty() {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query("UPDATE meetings SET title = ?, updated_at = ? WHERE id = ?")
            .bind(&title).bind(&now).bind(&meeting_id)
            .execute(state.db.pool()).await;
    }

    Ok(serde_json::json!({"ok": !title.is_empty(), "title": title}))
}

#[tauri::command]
pub async fn delete_meeting(state: State<'_, AppState>, meeting_id: String) -> Result<(), String> {
    // 级联删除会议相关的所有数据（事务包裹，要么全成功要么全回滚）
    let mut tx = state.db.pool().begin().await.map_err(|e| format!("开启事务失败: {}", e))?;

    sqlx::query("DELETE FROM chunk_summaries WHERE chunk_id IN (SELECT id FROM chunks WHERE meeting_id = ?)").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 chunk_summaries 失败: {}", e))?;
    sqlx::query("DELETE FROM chunks WHERE meeting_id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 chunks 失败: {}", e))?;
    sqlx::query("DELETE FROM final_summaries WHERE meeting_id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 final_summaries 失败: {}", e))?;
    sqlx::query("DELETE FROM jobs WHERE meeting_id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 jobs 失败: {}", e))?;
    sqlx::query("DELETE FROM speakers WHERE meeting_id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 speakers 失败: {}", e))?;
    sqlx::query("DELETE FROM correction_samples WHERE meeting_id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 correction_samples 失败: {}", e))?;
    sqlx::query("DELETE FROM meetings WHERE id = ?").bind(&meeting_id).execute(&mut *tx).await.map_err(|e| format!("删 meetings 失败: {}", e))?;

    tx.commit().await.map_err(|e| format!("提交事务失败: {}", e))?;

    // 清理空的录音会话目录（持久化录音路径下 rec_*，仅删无音频段的空壳目录，避免误删进行中的录音）
    let temp_dir = format!("{}/Library/Application Support/笔尖/audio_cache", std::env::var("HOME").unwrap_or_default());
    if let Ok(entries) = std::fs::read_dir(temp_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && path.file_name().map(|n| n.to_string_lossy().starts_with("rec_")).unwrap_or(false) {
                let has_seg = std::fs::read_dir(&path)
                    .map(|mut it| it.any(|e| e.as_ref().map(|f| f.file_name().to_string_lossy().starts_with("seg_")).unwrap_or(false)))
                    .unwrap_or(true);
                if !has_seg {
                    let _ = std::fs::remove_dir_all(&path);
                }
            }
        }
    }

    Ok(())
}

#[tauri::command]
pub async fn upload_audio(state: State<'_, AppState>, meeting_id: String, file_path: String) -> Result<serde_json::Value, String> {
    // 音频格式检测
    let ext = file_path.rsplit('.').next().unwrap_or("").to_lowercase();
    let supported = ["wav", "mp3", "flac", "ogg", "m4a", "aac", "webm", "opus"];
    if !supported.contains(&ext.as_str()) {
        let msg = format!("不支持的音频格式 .{}，支持的格式: {}", ext, supported.join(", "));
        eprintln!("[upload_audio] 格式错误: {}", msg);
        return Err(msg);
    }
    eprintln!("[upload_audio] 文件: {}, 格式: .{}", file_path, ext);
    
    let bytes = tokio::fs::read(&file_path).await.map_err(|e| format!("读取文件失败: {}", e))?;
    eprintln!("[upload_audio] 文件大小: {} bytes ({:.1} MB)", bytes.len(), bytes.len() as f64 / 1048576.0);
    
    // v3: 单引擎架构，统一用 Qwen3-ASR（/qwen3_transcribe + context 热词）
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());
    let hotword_context = db_get_hotword_context(&state.db, &meeting_id).await;
    
    // 主引擎转写
    let url = if hotword_context.is_empty() {
        format!("{}/qwen3_transcribe", asr_url)
    } else {
        format!("{}/qwen3_transcribe?context={}", asr_url, urlencode(&hotword_context))
    };
    let form = reqwest::multipart::Form::new().part("file", reqwest::multipart::Part::bytes(bytes.clone()).file_name("audio.wav"));
    let resp = state.http_client.post(&url).multipart(form).send().await.map_err(|e| {
        eprintln!("[upload_audio] ASR 请求失败: {} (URL: {})", e, url);
        format!("ASR 服务连接失败，请确认 ASR 服务已启动 ({}): {}", asr_url, e)
    })?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("[upload_audio] ASR 返回错误状态 {}: {}", status, body);
        return Err(format!("ASR 服务返回错误 ({}): {}", status, body));
    }
    let result: serde_json::Value = resp.json().await.map_err(|e| format!("ASR 响应解析失败: {}", e))?;
    
    // 提取转写文本
    let mut text = result.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
    let duration = result.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
    let primary_engine = result.get("engine").and_then(|e| e.as_str()).unwrap_or("qwen3_asr").to_string();
    
    // 热词后处理纠正
    text = db_apply_hotwords(&text, &state.db).await;
    
    if !text.is_empty() {
        let cid = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count) VALUES (?, ?, ?, ?, ?, 'unknown', 0.9, ?)")
            .bind(&cid).bind(&meeting_id).bind(0.0).bind(duration).bind(&text).bind(text.len() as i64)
            .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
        
        // 自动创建 chunk_summary job
        let jid = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'chunk_summary', 'pending', ?)")
            .bind(&jid).bind(&meeting_id).bind(serde_json::json!({"chunk_id": cid}).to_string())
            .execute(state.db.pool()).await.ok();
    }
    
    let now = chrono::Utc::now().to_rfc3339();
    sqlx::query("UPDATE meetings SET status = 'transcribed', updated_at = ? WHERE id = ?").bind(&now).bind(&meeting_id).execute(state.db.pool()).await.ok();
    
    // 清理临时文件
    let _ = tokio::fs::remove_file(&file_path).await;
    
    let response = serde_json::json!({"ok": true, "text": text, "duration": duration, "engine": primary_engine});
    Ok(response)
}

#[tauri::command]
pub async fn trigger_aggregate(state: State<'_, AppState>, meeting_id: String, template_type: Option<String>, depth: Option<String>, force: Option<bool>) -> Result<serde_json::Value, String> {
    if !crate::feature::is_feature_enabled(crate::feature::FeatureFlag::MeetingSummary) {
        return Err("会议纪要功能需要 Pro 版".to_string());
    }

    // v2.4.4: 转写完整性守卫——转写未完成禁止生成纪要（根因修复：此前 aggregate 只检查
    // "已落库 chunk 是否处理完"，不检查"是否仍有新录音段在转写"，导致 18:28 的纪要只含前半段）
    // 1) 活跃录音中 → 拒绝
    {
        let rec_guard = recording_lock().lock().map_err(|e| e.to_string())?;
        if rec_guard.is_some() {
            return Err("正在录音，请结束录音后再生成纪要".to_string());
        }
    }
    // 2) 最近 120 秒内仍有新 chunk 落库 → 转写仍在进行 → 拒绝
    let recent_chunks = sqlx::query("SELECT COUNT(*) as cnt FROM chunks WHERE meeting_id = ? AND created_at >= datetime('now', '-120 seconds')")
        .bind(&meeting_id)
        .fetch_one(state.db.pool())
        .await
        .map(|r| r.get::<i64, _>("cnt"))
        .unwrap_or(0);
    if recent_chunks > 0 {
        return Err("转写尚未完成，仍在处理录音片段，请等转写完成后再生成纪要".to_string());
    }

    // 3) v2.5.2 段级完整性门槛（录音型会议，段级表有记录才管——导入型不在此列）：
    //    - pending/processing 未清零 → 转写仍在跑 → 一律拒绝（差集会补完，等一等）
    //    - 存在 failed 段 → 默认拒绝；force=true 强制放行，payload 带部分转写标注注入纪要头
    let seg = sqlx::query(
        "SELECT COUNT(*) as total, COALESCE(SUM(CASE WHEN state IN ('pending','processing') THEN 1 ELSE 0 END),0) as unfinished, COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0) as failed, COALESCE(SUM(CASE WHEN state='done' THEN 1 ELSE 0 END),0) as done FROM transcription_segments WHERE meeting_id = ?"
    )
    .bind(&meeting_id)
    .fetch_one(state.db.pool()).await
    .map_err(|e| e.to_string())?;
    let (seg_total, seg_unfinished, seg_failed, seg_done): (i64, i64, i64, i64) = (
        seg.get("total"), seg.get("unfinished"), seg.get("failed"), seg.get("done"));
    let mut partial_note = String::new();
    if seg_total > 0 {
        if seg_unfinished > 0 {
            return Err(format!("转写尚未完成（{} 段处理中），请等待或重试失败段后再生成纪要", seg_unfinished));
        }
        if seg_failed > 0 {
            if !force.unwrap_or(false) {
                return Err(format!("有 {} 个失败段，默认禁止生成纪要。请先重试失败段；确认要基于部分转写生成请重试确认（纪要将标注）", seg_failed));
            }
            partial_note = format!("【注意：本纪要基于部分转写生成（成功 {}/{} 段，另有 {} 段转写失败）】", seg_done, seg_total, seg_failed);
        }
    }

    let template = template_type.unwrap_or_else(|| "meeting".to_string());
    let depth_val = depth.unwrap_or_else(|| "standard".to_string());
    let job_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'aggregate', 'pending', ?)")
        .bind(&job_id).bind(&meeting_id).bind(serde_json::json!({"meeting_id": meeting_id, "template_type": template, "depth": depth_val, "partial_note": partial_note}).to_string())
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"ok": true, "job_id": job_id}))
}

/// P2: 触发 AI 纠错 — 用 Ollama qwen3-4b + 热词表逐段修正 ASR 转写文本
#[tauri::command]
pub async fn trigger_correction(state: State<'_, AppState>, meeting_id: String) -> Result<serde_json::Value, String> {
    if !crate::feature::is_feature_enabled(crate::feature::FeatureFlag::AsrPremium) {
        return Err("AI 纠错功能需要 Pro 版".to_string());
    }
    let job_id = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'correction', 'pending', ?)")
        .bind(&job_id).bind(&meeting_id).bind(serde_json::json!({"meeting_id": meeting_id}).to_string())
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"ok": true, "job_id": job_id}))
}

// ========== v2.5.2 纪要导出（.docx / .md）==========
// 财务条线纪要最终要进 OA——从"复制粘贴"到"直接交付"的临门一脚。
// 路径由前端 dialog 选（tauri-plugin-dialog 已有），本命令只组稿+写文件，不弹系统对话框。

/// 组 markdown 稿（docx 与 md 共用同一内容源；markdown 是纪要结构化字段的扁平化）
#[doc(hidden)]
pub fn summary_json_to_markdown(title: &str, created: &str, summary: &serde_json::Value) -> String {
    let mut md = format!("# {} 会议纪要\n\n- 会议时间：{}\n", title, created);
    if let Some(pn) = summary.get("partial_note").and_then(|p| p.as_str()) {
        if !pn.is_empty() { md.push_str(&format!("- {}\n", pn)); }
    }
    md.push('\n');
    if let Some(tldr) = summary.get("tldr").and_then(|t| t.as_str()) {
        if !tldr.is_empty() { md.push_str(&format!("## 概要\n\n{}\n\n", tldr)); }
    }
    if let Some(kps) = summary.get("key_points").and_then(|k| k.as_array()) {
        if !kps.is_empty() {
            md.push_str("## 核心结论\n\n");
            for kp in kps { if let Some(s) = kp.as_str() { md.push_str(&format!("- {}\n", s)); } }
            md.push('\n');
        }
    }
    if let Some(topics) = summary.get("topics").and_then(|t| t.as_array()) {
        if !topics.is_empty() {
            md.push_str("## 议题讨论\n\n");
            for t in topics {
                let tt = t.get("title").and_then(|x| x.as_str()).unwrap_or("");
                md.push_str(&format!("### {}\n", tt));
                if let Some(s) = t.get("summary").and_then(|x| x.as_str()) {
                    if !s.is_empty() { md.push_str(&format!("{}\n", s)); }
                }
                md.push('\n');
            }
        }
    }
    if let Some(actions) = summary.get("actions").and_then(|a| a.as_array()) {
        if !actions.is_empty() {
            md.push_str("## 行动项\n\n");
            for a in actions { if let Some(s) = a.as_str() { md.push_str(&format!("- {}\n", s)); } }
            md.push('\n');
        }
    }
    // record 模板：说话人分段
    if summary.get("record").and_then(|r| r.as_bool()).unwrap_or(false) {
        if let Some(content) = summary.get("content").and_then(|c| c.as_str()) {
            if !content.is_empty() {
                md.push_str("## 会议记录\n\n");
                for line in content.lines() {
                    let l = line.trim();
                    if !l.is_empty() { md.push_str(&format!("{}\n\n", l)); }
                }
            }
        }
        if let Some(lines) = summary.get("lines").and_then(|l| l.as_array()) {
            if !lines.is_empty() {
                md.push_str("## 说话人分段\n\n");
                for l in lines { if let Some(s) = l.as_str() { md.push_str(&format!("{}\n\n", s)); } }
            }
        }
    }
    // 非 JSON 兜底：原始 markdown
    if summary.get("tldr").is_none() && summary.get("record").is_none() {
        if let Some(raw) = summary.get("markdown").and_then(|m| m.as_str()) {
            md.push_str(raw);
        }
    }
    md
}

#[tauri::command]
pub async fn export_summary(state: State<'_, AppState>, meeting_id: String, format: String, file_path: String) -> Result<serde_json::Value, String> {
    // 1) 会议标题
    let title: String = sqlx::query("SELECT title FROM meetings WHERE id = ?")
        .bind(&meeting_id).fetch_optional(state.db.pool()).await
        .map_err(|e| e.to_string())?
        .map(|r| r.get::<String, _>("title"))
        .unwrap_or_else(|| "未命名会议".to_string());
    // 2) 会议创建时间
    let created: String = sqlx::query("SELECT created_at FROM meetings WHERE id = ?")
        .bind(&meeting_id).fetch_optional(state.db.pool()).await
        .map_err(|e| e.to_string())?
        .map(|r| r.get::<String, _>("created_at"))
        .unwrap_or_default();
    // 3) 最新纪要（同 get_summary 解析逻辑）
    let content: Option<String> = sqlx::query("SELECT content FROM final_summaries WHERE meeting_id = ? ORDER BY created_at DESC LIMIT 1")
        .bind(&meeting_id).fetch_optional(state.db.pool()).await
        .map_err(|e| e.to_string())?
        .map(|r| r.get::<String, _>("content"));
    let content = match content {
        Some(c) => c,
        None => return Err("纪要尚未生成，请先生成纪要再导出".to_string()),
    };
    let trimmed = content.trim();
    let json_str = if trimmed.starts_with("```") {
        trimmed.lines().skip(1).collect::<Vec<_>>().join("\n").trim_end_matches("```").trim().to_string()
    } else { content.clone() };
    let summary: serde_json::Value = serde_json::from_str(&json_str)
        .unwrap_or_else(|_| serde_json::json!({"markdown": content}));

    // 4) 组稿
    let md = summary_json_to_markdown(&title, &created, &summary);

    // 5) 写文件
    let path = std::path::PathBuf::from(&file_path);
    match format.as_str() {
        "md" => {
            tokio::fs::write(&path, md).await.map_err(|e| format!("写入 md 失败: {}", e))?;
        }
        "docx" => {
            // docx-rs 是同步库，文件小（几十 KB），放 blocking 线程避免卡 async runtime
            let p = path.clone();
            let doc = tokio::task::spawn_blocking(move || -> Result<(), String> {
                build_docx(&md, &p)
            }).await.map_err(|e| e.to_string())??;
            let _ = doc;
        }
        _ => return Err(format!("不支持的导出格式: {}（支持 docx / md）", format)),
    }
    crate::logger::log("Export", &format!("纪要导出完成：{} → {:?}", format, path));
    Ok(serde_json::json!({ "ok": true, "path": file_path, "format": format }))
}

/// markdown 简易结构 → docx（只解析本命令组稿产出的固定结构：#/##/### 标题、- 列表、其余正文）
#[doc(hidden)]
pub fn build_docx(md: &str, path: &std::path::Path) -> Result<(), String> {
    use docx_rs::*;
    let mut doc = Docx::new();
    for line in md.lines() {
        let t = line.trim();
        if t.is_empty() { continue; }
        if let Some(h1) = t.strip_prefix("# ") {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(h1).bold().size(44)));
        } else if let Some(h2) = t.strip_prefix("## ") {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(h2).bold().size(32)));
        } else if let Some(h3) = t.strip_prefix("### ") {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(h3).bold().size(28)));
        } else if let Some(li) = t.strip_prefix("- ") {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(format!("• {}", li))));
        } else {
            doc = doc.add_paragraph(Paragraph::new().add_run(Run::new().add_text(t)));
        }
    }
    let file = std::fs::File::create(path).map_err(|e| format!("创建 docx 失败: {}", e))?;
    doc.build().pack(file).map_err(|e| format!("docx 打包失败: {}", e))?;
    Ok(())
}

#[tauri::command]
pub async fn get_summary(state: State<'_, AppState>, meeting_id: String) -> Result<serde_json::Value, String> {
    let rows = sqlx::query("SELECT id, meeting_id, content, summary_type, created_at FROM final_summaries WHERE meeting_id = ? ORDER BY created_at DESC LIMIT 1")
        .bind(&meeting_id).fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
    
    match rows {
        Some(row) => {
            let content: String = row.get("content");
            // 剥离 markdown 围栏（```json ... ```）
            let trimmed = content.trim();
            let json_str = if trimmed.starts_with("```") {
                // 去掉首行 ```json 或 ```，去掉末尾 ```
                let inner = trimmed
                    .lines()
                    .skip(1) // 跳过 ```json 首行
                    .collect::<Vec<_>>()
                    .join("\n");
                let inner = inner.trim_end_matches("```").trim();
                inner.to_string()
            } else {
                content.clone()
            };
            // 尝试解析为 JSON（Worker 生成时是 JSON 格式）
            let parsed = serde_json::from_str::<serde_json::Value>(&json_str).unwrap_or_else(|_| {
                serde_json::json!({"markdown": content})
            });
            Ok(parsed)
        }
        None => Err("纪要尚未生成".to_string()),
    }
}

#[tauri::command]
pub async fn get_transcript(state: State<'_, AppState>, meeting_id: String) -> Result<Vec<TranscriptChunk>, String> {
    let rows = sqlx::query("SELECT id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count FROM chunks WHERE meeting_id = ? ORDER BY start_time ASC").bind(&meeting_id).fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(rows.iter().map(|r| TranscriptChunk { id: r.get("id"), meeting_id: r.get("meeting_id"), start_time: r.get::<f64,_>("start_time"), end_time: r.get::<f64,_>("end_time"), text: r.get("transcript"), speaker: r.get("speaker"), confidence: r.get::<f64,_>("confidence"), word_count: r.get::<i64,_>("word_count") }).collect())
}

#[tauri::command]
pub async fn edit_transcript_chunk(state: State<'_, AppState>, meeting_id: String, chunk_id: String, text: String) -> Result<serde_json::Value, String> {
    // P0-2: 编辑前先取原文，用于 diff 提取热词
    // P1-2: 同时取 speaker/confidence/start_time，用于写 correction_samples
    let chunk_info = sqlx::query("SELECT transcript, speaker, confidence, start_time FROM chunks WHERE id = ? AND meeting_id = ?")
        .bind(&chunk_id).bind(&meeting_id)
        .fetch_one(state.db.pool()).await
        .ok();

    let old_text: String = chunk_info.as_ref()
        .map(|r| r.get::<String, _>("transcript"))
        .unwrap_or_default();
    let speaker: String = chunk_info.as_ref()
        .map(|r| r.get::<String, _>("speaker"))
        .unwrap_or_else(|| "unknown".to_string());
    let confidence: f64 = chunk_info.as_ref()
        .map(|r| r.get::<f64, _>("confidence"))
        .unwrap_or(0.0);
    let start_time: f64 = chunk_info.as_ref()
        .map(|r| r.get::<f64, _>("start_time"))
        .unwrap_or(0.0);

    let wc = text.len() as i64;
    sqlx::query("UPDATE chunks SET transcript = ?, word_count = ?, processed_flag = 0 WHERE id = ? AND meeting_id = ?").bind(&text).bind(wc).bind(&chunk_id).bind(&meeting_id).execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    sqlx::query("DELETE FROM chunk_summaries WHERE chunk_id = ?").bind(&chunk_id).execute(state.db.pool()).await.ok();
    sqlx::query("DELETE FROM final_summaries WHERE meeting_id = ?").bind(&meeting_id).execute(state.db.pool()).await.ok();
    let jid = Uuid::new_v4().to_string();
    sqlx::query("INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'chunk_summary', 'pending', ?)").bind(&jid).bind(&meeting_id).bind(serde_json::json!({"chunk_id": chunk_id}).to_string()).execute(state.db.pool()).await.map_err(|e| e.to_string())?;

    // P1-2: 写入纠错样本（ASR 原文 → 用户修正，含音频路径/说话人/置信度）
    // 只有原文非空且与修正不同时才记录
    let mut sample_saved = false;
    if !old_text.is_empty() && old_text != text {
        // 取 meeting title 和 session_dir
        let meeting_info = sqlx::query("SELECT title, session_dir FROM meetings WHERE id = ?")
            .bind(&meeting_id)
            .fetch_optional(state.db.pool()).await
            .ok()
            .flatten();

        let (meeting_title, session_dir) = match &meeting_info {
            Some(r) => (
                r.try_get::<String, _>("title").unwrap_or_default(),
                r.try_get::<String, _>("session_dir").unwrap_or_default(),
            ),
            None => (String::new(), String::new()),
        };

        // 拼接音频片段路径：seg_{:03}.wav
        let seg_num = (start_time / SEG_DURATION_SEC).floor() as u32;
        let audio_path = if !session_dir.is_empty() {
            format!("{}/seg_{:03}.wav", session_dir, seg_num)
        } else {
            String::new()
        };

        // 场景推断：用会议标题做简单关键词匹配
        let scene = infer_scene(&meeting_title);

        let sample_id = Uuid::new_v4().to_string();
        let _ = sqlx::query(
            "INSERT INTO correction_samples (id, meeting_id, chunk_id, asr_text, corrected_text, audio_path, speaker, scene, model, confidence)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
        )
        .bind(&sample_id)
        .bind(&meeting_id)
        .bind(&chunk_id)
        .bind(&old_text)
        .bind(&text)
        .bind(&audio_path)
        .bind(&speaker)
        .bind(&scene)
        .bind("qwen3_asr")  // 当前唯一引擎
        .bind(confidence)
        .execute(state.db.pool()).await;
        sample_saved = true;
    }

    // P0-2: 自动 diff 提取纠错热词
    // 策略：按句切分（中文标点），逐句比较原文和修订，提取不同的片段作为 wrong→correct
    let auto_count = auto_extract_hotwords(&old_text, &text, &state).await;

    let msg = if auto_count > 0 {
        format!("已更新，自动提取 {} 条纠错热词{}",
            auto_count,
            if sample_saved { "，纠错样本已记录" } else { "" }
        )
    } else if sample_saved {
        "已更新，纠错样本已记录".to_string()
    } else {
        "已更新".to_string()
    };
    Ok(serde_json::json!({"ok": true, "message": msg, "hotwords_extracted": auto_count, "sample_saved": sample_saved}))
}

/// P1-2: 根据会议标题推断场景
/// 简单关键词匹配，后续可扩展为更智能的方案
fn infer_scene(title: &str) -> &'static str {
    let title_lower = title.to_lowercase();
    if title_lower.contains("财务") || title_lower.contains("预算") || title_lower.contains("回款")
        || title_lower.contains("营收") || title_lower.contains("会计") || title_lower.contains("税务") {
        "finance"
    } else if title_lower.contains("项目") || title_lower.contains("需求") || title_lower.contains("开发")
        || title_lower.contains("技术") || title_lower.contains("架构") {
        "tech"
    } else if title_lower.contains("合同") || title_lower.contains("法务") || title_lower.contains("合规") {
        "legal"
    } else if title_lower.contains("培训") || title_lower.contains("学习") {
        "education"
    } else {
        "general"
    }
}

/// P1-4: 查询纠错样本统计（总数 + 最常错词 Top 10）
/// 供前端热词面板展示
#[tauri::command]
pub async fn get_correction_stats(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    // 总样本数
    let total: i64 = sqlx::query("SELECT COUNT(*) as c FROM correction_samples")
        .fetch_one(state.db.pool()).await
        .map(|r| r.get::<i64, _>("c"))
        .unwrap_or(0);

    // 按场景分布
    let scene_rows = sqlx::query(
        "SELECT scene, COUNT(*) as c FROM correction_samples GROUP BY scene ORDER BY c DESC"
    )
    .fetch_all(state.db.pool()).await.unwrap_or_default();

    let scenes: Vec<serde_json::Value> = scene_rows.iter().map(|r| {
        serde_json::json!({
            "scene": r.get::<String, _>("scene"),
            "count": r.get::<i64, _>("c"),
        })
    }).collect();

    // 最常错词 Top 10：从 hotwords 表按 frequency 排序（user_correction 来源优先）
    let top_rows = sqlx::query(
        "SELECT wrong_text, correct_text, frequency, scene FROM hotwords WHERE source = 'user_correction' ORDER BY frequency DESC, created_at DESC LIMIT 10"
    )
    .fetch_all(state.db.pool()).await.unwrap_or_default();

    let top_errors: Vec<serde_json::Value> = top_rows.iter().map(|r| {
        serde_json::json!({
            "wrong": r.get::<String, _>("wrong_text"),
            "correct": r.get::<String, _>("correct_text"),
            "frequency": r.get::<i64, _>("frequency"),
            "scene": r.get::<String, _>("scene"),
        })
    }).collect();

    // 最近纠错记录 5 条
    let recent_rows = sqlx::query(
        "SELECT asr_text, corrected_text, created_at FROM correction_samples ORDER BY created_at DESC LIMIT 5"
    )
    .fetch_all(state.db.pool()).await.unwrap_or_default();

    let recent: Vec<serde_json::Value> = recent_rows.iter().map(|r| {
        serde_json::json!({
            "asr_text": r.get::<String, _>("asr_text"),
            "corrected_text": r.get::<String, _>("corrected_text"),
            "created_at": r.get::<String, _>("created_at"),
        })
    }).collect();

    Ok(serde_json::json!({
        "total_samples": total,
        "scenes": scenes,
        "top_errors": top_errors,
        "recent_corrections": recent,
    }))
}

// ========== v2.6.0 纠错反哺闭环（数据飞轮）==========
// 现状：编辑转写时即时学热词已有（auto_extract_hotwords），
// 但 correction_samples 存量样本从未反哺热词——收了数据没转成产品力（v3 方案短板#6）。
// 本组命令把历史纠错样本批量蒸馏成热词建议，用户确认后一键入库：
// 下次转写 context 携带 + 后处理替换双路生效（db_get_hotword_context / db_apply_hotwords）。

/// 从 correction_samples 存量样本蒸馏热词建议：按 wrong→correct 聚合计数，
/// 过滤已入库热词，按频次排序返回。纯读操作，不落库——用户确认才应用。
#[tauri::command]
pub async fn get_correction_hotword_suggestions(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let rows: Vec<(String, String, String)> = sqlx::query(
        "SELECT asr_text, corrected_text, scene FROM correction_samples ORDER BY created_at DESC LIMIT 500"
    )
    .fetch_all(state.db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<String,_>("asr_text"), r.get::<String,_>("corrected_text"), r.get::<String,_>("scene"))).collect())
    .map_err(|e| e.to_string())?;

    // 已入库的 wrong→correct 对（去重基准）
    let existing: std::collections::HashSet<(String, String)> = sqlx::query("SELECT wrong_text, correct_text FROM hotwords")
        .fetch_all(state.db.pool()).await
        .map(|rs| rs.iter().map(|r| (r.get::<String,_>("wrong_text"), r.get::<String,_>("correct_text"))).collect())
        .unwrap_or_default();

    let mut counter: std::collections::HashMap<(String, String), (i64, String)> = std::collections::HashMap::new();
    for (asr, corrected, scene) in &rows {
        let old_segs = split_segments(asr);
        let new_segs = split_segments(corrected);
        if old_segs.len() != new_segs.len() { continue; }
        for (o, n) in old_segs.iter().zip(new_segs.iter()) {
            if o == n || o.is_empty() || n.is_empty() { continue; }
            if let Some((wrong, correct)) = extract_diff_pair(o, n) {
                let wl = wrong.chars().count(); let cl = correct.chars().count();
                if !(2..=60).contains(&wl) || !(1..=60).contains(&cl) { continue; }
                let e = counter.entry((wrong, correct)).or_insert((0, scene.clone()));
                e.0 += 1;
            }
        }
    }
    // 排序：频次降序；过滤已存在
    let mut suggestions: Vec<serde_json::Value> = counter.into_iter()
        .filter(|((w, c), _)| !existing.contains(&((*w).clone(), (*c).clone())))
        .map(|((w, c), (freq, scene))| serde_json::json!({
            "wrong": w, "correct": c, "frequency": freq, "scene": scene,
        }))
        .collect();
    suggestions.sort_by(|a, b| b["frequency"].as_i64().cmp(&a["frequency"].as_i64()));
    suggestions.truncate(50);
    Ok(serde_json::json!({ "suggestions": suggestions, "total": suggestions.len() }))
}

/// 一键应用纠错热词建议：入库 source='user_correction'，confidence=0.8（人工确认过的机器建议）
#[tauri::command]
pub async fn apply_correction_hotwords(state: State<'_, AppState>, suggestions: Vec<serde_json::Value>) -> Result<serde_json::Value, String> {
    let mut applied = 0i64;
    let mut skipped = 0i64;
    for s in &suggestions {
        let wrong = s.get("wrong").and_then(|v| v.as_str()).unwrap_or("");
        let correct = s.get("correct").and_then(|v| v.as_str()).unwrap_or("");
        if wrong.is_empty() || correct.is_empty() || wrong == correct { skipped += 1; continue; }
        let existing = sqlx::query("SELECT id FROM hotwords WHERE wrong_text = ? AND correct_text = ?")
            .bind(wrong).bind(correct)
            .fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
        if existing.is_some() { skipped += 1; continue; }
        let id = Uuid::new_v4().to_string();
        sqlx::query("INSERT INTO hotwords (id, wrong_text, correct_text, priority, source, scene, frequency, confidence) VALUES (?, ?, ?, 5, 'user_correction', 'general', 1, 0.8)")
            .bind(&id).bind(wrong).bind(correct)
            .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
        applied += 1;
    }
    crate::logger::log("CorrectionFeedback", &format!("纠错反哺：应用 {} 条热词（跳过 {} 条），下次转写生效", applied, skipped));
    Ok(serde_json::json!({ "applied": applied, "skipped": skipped }))
}

/// P0-2: 自动 diff 提取纠错热词
/// 对比 ASR 原文和用户修正文本，按句切分后逐句比较
/// 如果某句只有局部修改（替换了几个字），提取 wrong→correct 写入 hotwords
async fn auto_extract_hotwords(old_text: &str, new_text: &str, state: &State<'_, AppState>) -> usize {
    if old_text.is_empty() || new_text.is_empty() || old_text == new_text {
        return 0;
    }

    // 按标点切分成句片段
    let old_segs = split_segments(old_text);
    let new_segs = split_segments(new_text);

    if old_segs.len() != new_segs.len() {
        // 句数不同（用户可能增删了整句），跳过自动提取避免误学习
        return 0;
    }

    let mut count = 0;
    for (old_seg, new_seg) in old_segs.iter().zip(new_segs.iter()) {
        if old_seg == new_seg || old_seg.is_empty() || new_seg.is_empty() {
            continue;
        }
        // 两句都非空且不同，尝试提取最小差异片段
        if let Some((wrong, correct)) = extract_diff_pair(old_seg, new_seg) {
            // 只保存有意义的纠错（长度 2-20，避免太短或太长）
            if wrong.len() >= 2 && wrong.len() <= 60 && correct.len() >= 1 && correct.len() <= 60 {
                // 检查是否已存在相同的 wrong→correct
                let existing = sqlx::query("SELECT id FROM hotwords WHERE wrong_text = ? AND correct_text = ?")
                    .bind(&wrong).bind(&correct)
                    .fetch_optional(state.db.pool()).await.ok().flatten();
                if existing.is_none() {
                    let id = Uuid::new_v4().to_string();
                    let _ = sqlx::query(
                        "INSERT INTO hotwords (id, wrong_text, correct_text, priority, source, scene, frequency, confidence) VALUES (?, ?, ?, 5, 'user_correction', 'general', 1, 0.8)"
                    ).bind(&id).bind(&wrong).bind(&correct)
                     .execute(state.db.pool()).await;
                    count += 1;
                }
            }
        }
    }
    count
}

/// 按中文标点切分成句片段（保留片段文本，去掉标点）
fn split_segments(text: &str) -> Vec<String> {
    text.split(|c: char| matches!(c, '。' | '，' | '！' | '？' | '；' | '\n' | '.' | ',' | '!' | '?' | ';'))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// 提取两个字符串之间的最小差异片段
/// 如果两串长度相近且有公共前后缀，提取中间不同部分作为 wrong→correct
fn extract_diff_pair(old_seg: &str, new_seg: &str) -> Option<(String, String)> {
    let old_chars: Vec<char> = old_seg.chars().collect();
    let new_chars: Vec<char> = new_seg.chars().collect();

    // 找公共前缀
    let mut prefix_len = 0;
    while prefix_len < old_chars.len() && prefix_len < new_chars.len()
        && old_chars[prefix_len] == new_chars[prefix_len]
    {
        prefix_len += 1;
    }

    // 找公共后缀
    let mut suffix_len = 0;
    while suffix_len < old_chars.len() - prefix_len
        && suffix_len < new_chars.len() - prefix_len
        && old_chars[old_chars.len() - 1 - suffix_len] == new_chars[new_chars.len() - 1 - suffix_len]
    {
        suffix_len += 1;
    }

    let wrong: String = old_chars[prefix_len..old_chars.len() - suffix_len].iter().collect();
    let correct: String = new_chars[prefix_len..new_chars.len() - suffix_len].iter().collect();

    if wrong.is_empty() || correct.is_empty() {
        return None;
    }

    Some((wrong, correct))
}

// ========== ASR ==========

#[tauri::command]
pub async fn asr_status(state: State<'_, AppState>) -> Result<AsrStatusResponse, String> {
    let primary_running = state.asr_manager.is_running();
    let secondary_running = state.asr_manager.get_secondary_asr_url().is_some();
    let any_running = primary_running || secondary_running;
    
    let mut backends = vec![
        AsrBackendStatus { 
            name: "firered".to_string(), 
            available: primary_running, 
            error: if primary_running { None } else { Some("未连接".to_string()) } 
        },
    ];
    
    if secondary_running {
        backends.push(AsrBackendStatus { 
            name: "sensevoice".to_string(), 
            available: true, 
            error: None 
        });
    }
    
    // 检查 whisper.cpp 是否可用
    let whisper_available = std::path::Path::new("/usr/local/bin/whisper-cli").exists() && {
        let model_dirs = [
            format!("{}/.cache/whisper-models/ggml-tiny.bin", std::env::var("HOME").unwrap_or_default()),
            format!("{}/.cache/whisper-models/ggml-base.bin", std::env::var("HOME").unwrap_or_default()),
        ];
        model_dirs.iter().any(|p| std::path::Path::new(p).exists())
    };
    if whisper_available {
        backends.push(AsrBackendStatus { 
            name: "whisper".to_string(), 
            available: true, 
            error: None 
        });
    }
    
    Ok(AsrStatusResponse { 
        backends, 
        primary: "firered".to_string(),
        running: any_running,
    })
}

#[tauri::command]
pub async fn asr_ensure_running(state: State<'_, AppState>) -> Result<(), String> {
    state.asr_manager.ensure_running()
}

/// whisper.cpp 双引擎交叉验证
/// 检测 whisper-cli 和模型文件是否存在，有则跑，无则返回空
#[allow(dead_code)]
async fn run_whisper_cpp(wav_path: &str) -> Result<String, String> {
    let whisper_cli = std::process::Command::new("which")
        .arg("whisper-cli")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/bin/whisper-cli".to_string());

    if !std::path::Path::new(&whisper_cli).exists() {
        return Ok(String::new()); // whisper 未安装，跳过
    }

    // 查找模型文件
    let model_dirs = [
        std::env::var("WHISPER_MODEL_PATH").unwrap_or_default(),
        format!("{}/.cache/whisper-models/ggml-tiny.bin", std::env::var("HOME").unwrap_or_default()),
        format!("{}/.cache/whisper-models/ggml-base.bin", std::env::var("HOME").unwrap_or_default()),
    ];

    let model_path = model_dirs.iter().find(|p| !p.is_empty() && std::path::Path::new(p).exists());
    let model_path = match model_path {
        Some(p) => p.clone(),
        None => return Ok(String::new()), // 模型未下载，跳过
    };

    let output = std::process::Command::new(&whisper_cli)
        .arg("-m").arg(&model_path)
        .arg("-f").arg(wav_path)
        .arg("-l").arg("zh")
        .arg("-nt") // 不输出时间戳
        .arg("--output-txt").arg("-") // 输出到 stdout
        .output()
        .map_err(|e| format!("whisper 执行失败: {}", e))?;

    let text = String::from_utf8_lossy(&output.stdout)
        .trim()
        .to_string();
    Ok(text)
}

// ========== ASR launchd 常驻（OS 级，开机自启，重启不丢）==========
//
// 把 sherpa/firered ASR 服务包装成用户级 LaunchAgent：
//   ~/Library/LaunchAgents/com.bijian.asr.agent.plist
// 安装：asr_install_launchd → 写 plist → launchctl bootstrap → 启动
// 卸载：asr_uninstall_launchd → bootout → rm plist → kill 遗留进程
//
// 相对于 app 内部 start_in_background：launchd 能保证"开机即启动、崩溃自重启"，
// 是真正的"产品级"常驻。app 内 keep-alive 只在 app 运行时存在，关 app 就被杀；
// launchd agent 在 app 退出后照样可用，是"本地 AI 工作台"的基础设施。

#[derive(serde::Serialize, serde::Deserialize)]
pub struct LaunchdStatus {
    installed: bool,
    running: bool,
    plist_path: String,
    pid: Option<i32>,
    last_exit: Option<i32>,
}

pub fn asr_launchd_plist_path() -> std::path::PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    std::path::PathBuf::from(home).join("Library/LaunchAgents/com.bijian.asr.agent.plist")
}

fn asr_project_dir() -> String {
    std::env::var("BIJIAN_PROJECT_DIR").unwrap_or_else(|_| {
        // 产品版：不含开发者私有路径，默认取 App 数据目录下的 asr/
        // 用户可通过 BIJIAN_PROJECT_DIR 环境变量指定自定义路径
        format!("{}/Library/Application Support/PanNote/asr", std::env::var("HOME").unwrap_or_default())
    })
}

fn asr_python_path() -> String {
    std::env::var("BIJIAN_PYTHON").unwrap_or_else(|_| {
        "python3".to_string() // 产品版：不含开发者私有路径，用系统 PATH 中的 python3
    })
}

/// wrapper 脚本路径：放到 ~/Library/Application Support/PanNote/ 下，
/// 避免 ~/Documents 的 TCC 限制导致 launchd 无法执行
pub fn asr_wrapper_path() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    format!("{}/Library/Application Support/PanNote/asr_wrapper.sh", home)
}

/// ASR 日志/PID 目录（应用私有，不再用 /tmp）
fn asr_log_dir_for_commands() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    let dir = format!("{}/Library/Application Support/PanNote/asr_logs", home);
    std::fs::create_dir_all(&dir).ok();
    dir
}

/// 生成 launchd plist 内容
fn render_asr_agent_plist() -> String {
    let proj = asr_project_dir();
    let py = asr_python_path();
    let log_dir = asr_log_dir_for_commands();
    let sherpa_log = format!("{}/bijian_launchd_sherpa.log", log_dir);
    let firered_log = format!("{}/bijian_launchd_firered.log", log_dir);
    let sherpa_err = format!("{}/bijian_launchd_sherpa_err.log", log_dir);
    let firered_err = format!("{}/bijian_launchd_firered_err.log", log_dir);
    // 用一个包装脚本同时起 sherpa + firered：launchd 只允许一个 ProgramArguments
    let sh = format!(r##"#!/bin/bash
# 笔尖 ASR 常驻 wrapper：拉起 sherpa(:8083) + firered(:8082)
# 双重拉起防护（v2.3.2）：
#   - 启动前先 lsof 检测端口，被占说明已有 ASR 实例在跑 → 退出 0 不重启（防 KeepAlive 死循环）
#   - KeepAlive 改 SuccessfulExit=false：退出码 0 时 launchd 不重启，根治端口冲突空转
# v2.5.0（9-25）：日志改追加写 + 超 5MB 轮转——根治 launchd 重启清零案发现场的问题
set -u
export PATH="/usr/local/bin:/opt/homebrew/bin:$PATH"
PROJ="{proj}"
PY="{py}"

# 日志轮转：超 5MB 归档为 .1（保留上一份）
rotate_log() {{
  if [ -f "$1" ] && [ "$(stat -f%z "$1" 2>/dev/null || echo 0)" -gt 5242880 ]; then
    mv "$1" "$1.1"
  fi
}}
rotate_log "{sherpa_log}"; rotate_log "{sherpa_err}"
rotate_log "{firered_log}"; rotate_log "{firered_err}"
echo "[$(date '+%F %T')] ===== ASR wrapper 启动（v2.5.0 追加模式） REALTIME_ENGINE=$BIJIAN_REALTIME_ENGINE =====" >>"{sherpa_log}"

# 端口检测：任一端口已监听 → 退出 0（让原有 ASR 继续，launchd 不重启）
if lsof -i :8083 -i :8082 -P -n 2>/dev/null | grep -q LISTEN; then
  echo "[$(date '+%F %T')] 端口 8082/8083 已被占用，假定 ASR 已在跑，wrapper 退出 0" >>"{sherpa_log}"
  exit 0
fi

"$PY" "$PROJ/sherpa_asr_server.py" >>"{sherpa_log}" 2>>"{sherpa_err}" &
PID_SHERPA=$!
"$PY" "$PROJ/firered_server.py" >>"{firered_log}" 2>>"{firered_err}" &
PID_FIRERED=$!
# 任一子进程异常退出 → wrapper 退出 1 → launchd 30s 后重启
trap "kill $PID_SHERPA $PID_FIRERED 2>/dev/null; exit 1" TERM INT
while true; do
  if ! kill -0 "$PID_SHERPA" 2>/dev/null; then
    echo "[$(date '+%F %T')] sherpa 进程退出，wrapper 退出 1 等待 launchd 重启" >>"{sherpa_log}"
    exit 1
  fi
  if ! kill -0 "$PID_FIRERED" 2>/dev/null; then
    echo "[$(date '+%F %T')] firered 进程退出，wrapper 退出 1 等待 launchd 重启" >>"{firered_log}"
    exit 1
  fi
  sleep 5
done
"##, proj=proj, py=py, sherpa_log=sherpa_log, firered_log=firered_log, sherpa_err=sherpa_err, firered_err=firered_err);
    // 把 wrapper 脚本写到 ~/Library/Application Support/PanNote/（避免 ~/Documents 的 TCC 限制）
    let sh_path = asr_wrapper_path();
    if let Some(parent) = std::path::Path::new(&sh_path).parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let _ = std::fs::write(&sh_path, sh);
    let _ = std::process::Command::new("chmod").args(["+x", &sh_path]).status();

    let user = std::env::var("USER").unwrap_or_default();
    let stderr = format!("{}/bijian_asr_launchd.err", log_dir);
    let stdout = format!("{}/bijian_asr_launchd.out", log_dir);
    format!(r##"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.bijian.asr.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>/bin/bash</string>
        <string>{sh}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>OtherJobEnabled</key>
        <false/>
        <key>SuccessfulExit</key>
        <false/>
        <key>AfterInitialDemand</key>
        <true/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>30</integer>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
    <key>UserName</key>
    <string>{user}</string>
    <key>EnvironmentVariables</key>
    <dict>
        <key>BIJIAN_REALTIME_ENGINE</key>
        <string>paraformer</string>
    </dict>
    <key>ProcessType</key>
    <string>Background</string>
    <key>Nice</key>
    <integer>5</integer>
</dict>
</plist>
"##, sh=sh_path, stdout=stdout, stderr=stderr, user=user)
}

#[tauri::command]
pub async fn asr_launchd_status() -> Result<LaunchdStatus, String> {
    let p = asr_launchd_plist_path();
    let installed = p.exists();
    let mut running = false;
    let mut pid: Option<i32> = None;
    let mut last_exit: Option<i32> = None;
    // 查 launchctl 服务状态（$(id -u) 不经 shell 展开，需 Rust 获取 UID）
    let uid = std::process::Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(501); // fallback: 首个普通用户
    let domain = format!("gui/{}/com.bijian.asr.agent", uid);
    let out = std::process::Command::new("launchctl")
        .args(["print", &domain])
        .output()
        .ok();
    if let Some(o) = out {
        let s = String::from_utf8_lossy(&o.stdout);
        for line in s.lines() {
            let line = line.trim();
            if let Some(v) = line.strip_prefix("pid = ") { pid = v.parse().ok(); running = true; }
            if let Some(v) = line.strip_prefix("last exit code = ") {
                last_exit = v.split_whitespace().next().and_then(|x| x.parse().ok());
            }
        }
    }
    Ok(LaunchdStatus { installed, running, plist_path: p.to_string_lossy().into(), pid, last_exit })
}

#[tauri::command]
pub async fn asr_install_launchd() -> Result<(), String> {
    let plist_path = asr_launchd_plist_path();
    if let Some(parent) = plist_path.parent() { std::fs::create_dir_all(parent).ok(); }
    std::fs::write(&plist_path, render_asr_agent_plist()).map_err(|e| e.to_string())?;
    // 先 bootout（若已存在），再 bootstrap
    let uid = unsafe { libc_getuid() };
    let service = format!("gui/{}/com.bijian.asr.agent", uid);
    let _ = std::process::Command::new("launchctl").args(["bootout", &service]).output();
    let s = std::process::Command::new("launchctl")
        .args(["bootstrap", &format!("gui/{}", uid), plist_path.to_str().unwrap()])
        .output().map_err(|e| e.to_string())?;
    if !s.status.success() {
        return Err(format!("launchctl bootstrap 失败: {}", String::from_utf8_lossy(&s.stderr)));
    }
    // kickstart 立即起一次
    let _ = std::process::Command::new("launchctl").args(["kickstart", "-k", &service]).output();
    Ok(())
}

#[tauri::command]
pub async fn asr_uninstall_launchd() -> Result<(), String> {
    let uid = unsafe { libc_getuid() };
    let service = format!("gui/{}/com.bijian.asr.agent", uid);
    let _ = std::process::Command::new("launchctl").args(["bootout", &service]).output();
    // kill 残留 ASR 进程
    for p in ["sherpa_asr_server.py", "firered_server.py", "asr_wrapper.sh"] {
        let r = std::process::Command::new("pkill").args(["-f", p]).output();
        if let Ok(o) = r { let _ = o; }
    }
    let p = asr_launchd_plist_path();
    if p.exists() { std::fs::remove_file(&p).map_err(|e| e.to_string())?; }
    Ok(())
}

// 跨平台取 uid 的轻量 unsafe wrapper（macOS always）
unsafe fn libc_getuid() -> u32 {
    #[link(name = "c")]
    extern "C" { fn getuid() -> u32; }
    getuid()
}

// ========== 录音（Rust 原生 cpal + hound）==========
//
// 用 cpal 在笔尖主进程内直接采集麦克风，从根上避免 ffmpeg 子进程 TCC 权限不继承问题。
// 每次 SEG_DURATION_SEC 自动切一个 WAV 段；后台转写通过 Notify + 文件扫描拿到段文件。
// stop_recording 时：pause cpal stream → 落盘最后一段 → 通知后台完成尾段 → join。

use std::sync::{Mutex, OnceLock, Arc, atomic::{AtomicBool, Ordering}};
use std::io::BufWriter;
use hound::{WavSpec, WavWriter, SampleFormat};

/// 线性插值重采样：把 input_rate 的 I16 PCM 降到 16000 Hz（单声道）。
/// 不依赖外部库，直接整数数学（速度足够、质量满足 ASR 要求）。
/// 如果 input_rate == 16000 直接原样返回。
fn resample_to_16k_i16_mono(
    input: &[i16],
    input_rate: u32,
    output_rate: u32,
) -> Vec<i16> {
    if input_rate == output_rate || input.is_empty() { return input.to_vec(); }
    let ratio = input_rate as f64 / output_rate as f64;
    let out_len = ((input.len() as f64) / ratio).ceil() as usize;
    let mut out: Vec<i16> = Vec::with_capacity(out_len.max(1));
    for i in 0..out_len {
        let exact = i as f64 * ratio;
        let lo = exact.floor() as usize;
        let frac = exact - (lo as f64);
        let a = input.get(lo).copied().unwrap_or(0) as f64;
        let b = input.get(lo + 1).copied().map(|x| x as f64).unwrap_or(a);
        let v = a + (b - a) * frac;
        out.push(v.clamp(-32768.0, 32767.0) as i16);
    }
    out
}

/// 从多通道 PCM 里提取第一声道（单声道）
fn extract_mono_i16(samples: &[i16], channels: u16) -> Vec<i16> {
    let ch = channels as usize;
    if ch <= 1 { return samples.to_vec(); }
    samples.iter().step_by(ch).copied().collect()
}

/// Send/Sync 包装 cpal::Stream（CoreAudio 原生指针，跨线程 drop 是实际安全的）
#[allow(dead_code)]
struct SendStream(cpal::Stream);
unsafe impl Send for SendStream {}
unsafe impl Sync for SendStream {}
impl Drop for SendStream {
    fn drop(&mut self) {
        // cpal::Stream drop 内部会停止，这里不做额外事
    }
}

#[allow(dead_code)]
struct WavSegment {
    writer: WavWriter<BufWriter<std::fs::File>>,
    written_samples: usize,
    seg_index: u32,
    session_dir: String,
}

impl WavSegment {
    fn create(session_dir: &str, idx: u32) -> Result<Self, String> {
        let spec = WavSpec {
            channels: 1, sample_rate: 16000, bits_per_sample: 16, sample_format: SampleFormat::Int,
        };
        let path = format!("{}/seg_{:03}.wav", session_dir, idx);
        let file = std::fs::File::create(&path).map_err(|e| format!("创建段文件 {} 失败: {}", path, e))?;
        Ok(Self {
            writer: WavWriter::new(BufWriter::new(file), spec).map_err(|e| e.to_string())?,
            written_samples: 0,
            seg_index: idx,
            session_dir: session_dir.to_string(),
        })
    }
    fn write(&mut self, samples: &[i16]) -> Result<(), String> {
        for s in samples { self.writer.write_sample(*s).map_err(|e| e.to_string())?; }
        self.written_samples += samples.len();
        Ok(())
    }
    fn should_rotate(&self, seg_samples: usize) -> bool { self.written_samples >= seg_samples }
    fn finish(self) -> Result<u32, String> {
        let idx = self.seg_index;
        let path = format!("{}/seg_{:03}.wav", self.session_dir, idx);
        match self.writer.finalize() {
            Ok(_) => {
                // v2.5.0: finalize 成功也验证 WAV 头（防御性——9-24 实测 seg_055 size=0）
                Self::verify_fix_wav_size(&path);
                Ok(idx)
            }
            Err(e) => {
                // finalize 失败：手工回写 RIFF/data size，避免末段成为“假 WAV”
                crate::logger::log("Recording", &format!("段 {} finalize 失败（{}），尝试手工修 WAV 头", idx, e));
                Self::verify_fix_wav_size(&path);
                Err(e.to_string())
            }
        }
    }

    /// 验证并修复 WAV 头的 size 字段（标准 44 字节头：RIFF size@4， data size@40）
    fn verify_fix_wav_size(path: &str) {
        use std::io::{Read, Seek, SeekFrom, Write};
        let Ok(mut f) = std::fs::OpenOptions::new().read(true).write(true).open(path) else { return };
        let Ok(meta) = f.metadata() else { return };
        let len = meta.len();
        if len < 44 { return; }
        let mut head = [0u8; 44];
        if f.read_exact(&mut head).is_err() || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" { return; }
        let riff_sz = u32::from_le_bytes([head[4], head[5], head[6], head[7]]);
        let data_sz = u32::from_le_bytes([head[40], head[41], head[42], head[43]]);
        let expect_riff = (len - 8) as u32;
        let expect_data = (len - 44) as u32;
        if riff_sz == expect_riff && data_sz == expect_data { return; }
        // 头异常：回写正确值
        let _ = f.seek(SeekFrom::Start(4));
        let _ = f.write_all(&expect_riff.to_le_bytes());
        let _ = f.seek(SeekFrom::Start(40));
        let _ = f.write_all(&expect_data.to_le_bytes());
        let _ = f.flush();
        crate::logger::log("Recording", &format!("WAV 头修复: {} (riff {}→{}, data {}→{})", path, riff_sz, expect_riff, data_sz, expect_data));
    }
}

/// 把一段 I16 PCM 写入当前段；写满则切段 → 建下一段 → 发 Notify 唤醒后台
fn write_i16_rotate(
    seg_arc: &Arc<Mutex<Option<WavSegment>>>,
    notify_arc: &Arc<tokio::sync::Notify>,
    session_dir: &str,
    input: &[i16],
    seg_samples: usize,
) {
    let mut remaining: &[i16] = input;
    while !remaining.is_empty() {
        let mut guard = match seg_arc.lock() { Ok(g) => g, Err(_) => return };
        let cur = match guard.as_mut() { Some(c) => c, None => return };
        let space = seg_samples.saturating_sub(cur.written_samples);
        if space == 0 {
            // 应该走下面 rotate 逻辑，但这里兜底：强行 finish
            drop(guard);
            do_rotate(seg_arc, notify_arc, session_dir);
            continue;
        }
        let take = remaining.len().min(space);
        if cur.write(&remaining[..take]).is_err() { return; }
        remaining = &remaining[take..];
        if cur.should_rotate(seg_samples) {
            drop(guard);
            do_rotate(seg_arc, notify_arc, session_dir);
        }
    }
}

fn do_rotate(
    seg_arc: &Arc<Mutex<Option<WavSegment>>>,
    notify_arc: &Arc<tokio::sync::Notify>,
    session_dir: &str,
) {
    let (prev_idx, prev_res) = {
        let mut g = seg_arc.lock().unwrap();
        let old = match g.take() { Some(o) => o, None => return };
        let idx = old.seg_index;
        let res = old.finish();
        (idx, res)
    };
    if prev_res.is_ok() { notify_arc.notify_waiters(); }
    let mut g = seg_arc.lock().unwrap();
    if g.is_none() {
        let next = match WavSegment::create(session_dir, prev_idx + 1) {
            Ok(w) => w,
            Err(e) => { eprintln!("[Recording] 新建段失败: {}", e); return; }
        };
        *g = Some(next);
    }
}

struct ActiveRecording {
    _stream: SendStream,                    // RAII + Send-safe（跨线程可携带）
    current_segment: Arc<Mutex<Option<WavSegment>>>,
    seg_notify: Arc<tokio::sync::Notify>,
    session_dir: String,
    meeting_id: String,
    cancel_flag: Arc<AtomicBool>,
    task_handle: Option<tauri::async_runtime::JoinHandle<()>>,
}

static RECORDING: OnceLock<Mutex<Option<ActiveRecording>>> = OnceLock::new();

fn recording_lock() -> &'static Mutex<Option<ActiveRecording>> {
    RECORDING.get_or_init(|| Mutex::new(None))
}

/// 构建 cpal 输入流：使用设备原生采样率（自动兼容 44.1k/48k 等 MacBook 内置麦），
/// 在回调里线性插值重采样到 16k 单声道写 WAV，输出格式严格匹配 sherpa/qwen-asr 要求。
/// 返回 Stream(RAII) + 当前段 Arc + 段写完 Notify
fn build_cpal_stream(
    session_dir: String,
) -> Result<(cpal::Stream, Arc<Mutex<Option<WavSegment>>>, Arc<tokio::sync::Notify>), String> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

    let host = cpal::default_host();
    let device = host.default_input_device()
        .ok_or("未找到麦克风。请在系统设置 → 隐私与安全性 → 麦克风里为「PanNote」授权后重试")?;
    eprintln!("[cpal] 使用设备: {}", device.name().unwrap_or_default());

    // 挑采样率：取设备范围内 ≥16k 且尽可能低（靠近 16k 重采样损失小），
    // 若设备最小 > 16k（MacBook 内置麦 44100 起步）就取最小。
    let supported_list: Vec<_> = device.supported_input_configs()
        .map_err(|e| format!("枚举麦克风配置失败: {}", e))?.collect();
    eprintln!("[cpal] 支持的配置数: {}", supported_list.len());
    for r in &supported_list {
        eprintln!("[cpal] 候选: ch={}, fmt={:?}, sr=[{},{}]",
            r.channels(), r.sample_format(), r.min_sample_rate().0, r.max_sample_rate().0);
    }
    let (fmt, chs, sample_rate) = pick_mic_config(supported_list)
        .ok_or("未找到可用的麦克风 PCM 配置（支持格式: I16/F32/U16）")?;
    eprintln!("[cpal] 选中: ch={}, fmt={:?}, sr={}", chs, fmt, sample_rate);

    let cfg = cpal::StreamConfig {
        channels: chs,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // 输出段采样数（一律按 16k 算，不管输入采样率）
    let seg_samples_16k: usize = 16000 * SEG_DURATION_SEC as usize;
    let current: Arc<Mutex<Option<WavSegment>>> = Arc::new(Mutex::new(Some(WavSegment::create(&session_dir, 0)?)));
    let notify: Arc<tokio::sync::Notify> = Arc::new(tokio::sync::Notify::new());

    let cur_cb = current.clone();
    let nfy_cb = notify.clone();
    let sd_cb = session_dir.clone();

    let err_fn = |e| eprintln!("[cpal] 录音流错误: {}", e);

    let stream = match fmt {
        cpal::SampleFormat::I16 => {
            device.build_input_stream(&cfg,
                move |d: &[i16], _| {
                    let mono = extract_mono_i16(d, cfg.channels);
                    let down = resample_to_16k_i16_mono(&mono, cfg.sample_rate.0, 16000);
                    write_i16_rotate(&cur_cb, &nfy_cb, &sd_cb, &down, seg_samples_16k);
                }, err_fn, None).map_err(|e| e.to_string())?
        }
        cpal::SampleFormat::F32 => {
            device.build_input_stream(&cfg,
                move |d: &[f32], _| {
                    // F32 → I16
                    let i16_raw: Vec<i16> = d.iter().map(|&f| (f.clamp(-1.0,1.0)*32767.0) as i16).collect();
                    let mono = extract_mono_i16(&i16_raw, cfg.channels);
                    let down = resample_to_16k_i16_mono(&mono, cfg.sample_rate.0, 16000);
                    write_i16_rotate(&cur_cb, &nfy_cb, &sd_cb, &down, seg_samples_16k);
                }, err_fn, None).map_err(|e| e.to_string())?
        }
        cpal::SampleFormat::U16 => {
            device.build_input_stream(&cfg,
                move |d: &[u16], _| {
                    let i16_raw: Vec<i16> = d.iter().map(|&u| (u as i32 - 32768) as i16).collect();
                    let mono = extract_mono_i16(&i16_raw, cfg.channels);
                    let down = resample_to_16k_i16_mono(&mono, cfg.sample_rate.0, 16000);
                    write_i16_rotate(&cur_cb, &nfy_cb, &sd_cb, &down, seg_samples_16k);
                }, err_fn, None).map_err(|e| e.to_string())?
        }
        f => return Err(format!("未支持的 PCM 样本格式: {:?}", f)),
    };

    stream.play().map_err(|e| format!("启动录音流失败（若提示权限拒绝请先授予「PanNote」麦克风权限）: {}", e))?;
    Ok((stream, current, notify))
}

/// 从 cpal 候选配置里挑最合适的：
/// 优先级：格式符合（I16/F32/U16）→ 声道数≥1 → 采样率尽可能接近 16k（且设备支持）
fn pick_mic_config(
    supported: Vec<cpal::SupportedStreamConfigRange>,
) -> Option<(cpal::SampleFormat, u16, u32)> {
    use cpal::SampleFormat::*;
    const TARGET: u32 = 16000;
    let mut best: Option<(cpal::SampleFormat, u16, u32, u64)> = None; // score 越低越好
    for r in supported {
        if r.channels() < 1 { continue; }
        let fmt = r.sample_format();
        if !matches!(fmt, I16 | F32 | U16) { continue; }
        let (lo, hi) = (r.min_sample_rate().0, r.max_sample_rate().0);
        let sr = if (lo..=hi).contains(&TARGET) { TARGET }
                 else if hi < TARGET { hi }
                 else { lo }; // TARGET < lo → 取设备最小可用
        let score = (sr as i64 - TARGET as i64).unsigned_abs()
            // 格式偏好：F32 第一（Mac 常见原生格式）> I16 > U16
            + 1_000_000 * match fmt { F32 => 0, I16 => 1, U16 => 2, _ => 9 };
        let chs = r.channels().min(2); // 单声道足够，双声道也行
        let key = (fmt, chs, sr, score);
        if best.as_ref().map_or(true, |b| key.3 < b.3) {
            best = Some(key);
        }
    }
    best.map(|(f, ch, sr, _)| (f, ch, sr))
}

// P0-3: 热词后处理 — 全角半角归一化 + 上下文保护替换
// 改进点：
// 1. 全角半角归一化：ASR 可能输出全角字母/数字，热词是半角，需统一匹配
// 2. 上下文保护：wrong_text 前后字符检查，避免误替换子串
//    例如 "网不层" 不应匹配 "网络不层次" 中的子串
async fn db_apply_hotwords(text: &str, db: &crate::db::Database) -> String {
    let rows = sqlx::query("SELECT wrong_text, correct_text FROM hotwords ORDER BY priority DESC, length(wrong_text) DESC")
        .fetch_all(db.pool()).await
        .unwrap_or_default();

    let mut result = text.to_string();
    for row in &rows {
        let wrong: String = row.get("wrong_text");
        let correct: String = row.get("correct_text");
        if wrong.is_empty() || wrong == correct {
            continue;
        }
        // 全角半角归一化后做替换
        let wrong_norm = normalize_fullwidth(&wrong);
        let result_norm = normalize_fullwidth(&result);
        // 找到归一化后所有匹配位置，映射回原文做替换
        // 简化策略：如果归一化后文本含归一化后热词，直接在原文中替换
        // 对于纯中文热词（最常见场景），全角半角不影响，直接 replace
        if result_norm.contains(&wrong_norm) {
            // 逐位置替换，避免子串误匹配
            result = replace_with_boundary_check(&result, &wrong, &correct);
        }
    }
    result
}

/// 全角→半角归一化（字母、数字、常见标点）
fn normalize_fullwidth(s: &str) -> String {
    s.chars().map(|c| {
        let code = c as u32;
        match code {
            // 全角空格 U+3000 → 半角空格
            0x3000 => ' ',
            // 全角字母 A-Z (U+FF21-FF3A) → 半角 A-Z
            0xFF21..=0xFF3A => char::from_u32(code - 0xFEE0).unwrap_or(c),
            // 全角字母 a-z (U+FF41-FF5A) → 半角 a-z
            0xFF41..=0xFF5A => char::from_u32(code - 0xFEE0).unwrap_or(c),
            // 全角数字 0-9 (U+FF10-FF19) → 半角 0-9
            0xFF10..=0xFF19 => char::from_u32(code - 0xFEE0).unwrap_or(c),
            _ => c,
        }
    }).collect()
}

/// 带边界检查的替换：只在 wrong 前后不是汉字/字母/数字时才替换
/// 避免"网不层"误匹配"网络不层次"这类子串问题
fn replace_with_boundary_check(text: &str, wrong: &str, correct: &str) -> String {
    if wrong.is_empty() || !text.contains(wrong) {
        return text.to_string();
    }
    // 如果 correct 和 wrong 长度不同，简单替换可能导致偏移
    // 对于热词场景（通常短词替换），直接 replace 是安全的
    // 但加前导/后继字符检查：如果 wrong 前后紧跟汉字/字母/数字，说明可能是更大词的一部分，跳过
    let wrong_chars: Vec<char> = wrong.chars().collect();
    let mut result = String::with_capacity(text.len());
    let text_chars: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < text_chars.len() {
        // 尝试在位置 i 匹配 wrong
        if i + wrong_chars.len() <= text_chars.len()
            && text_chars[i..i + wrong_chars.len()] == wrong_chars[..]
        {
            // 检查前导字符
            let prev_ok = i == 0 || !is_word_char(text_chars[i - 1]);
            // 检查后继字符
            let next_idx = i + wrong_chars.len();
            let next_ok = next_idx >= text_chars.len() || !is_word_char(text_chars[next_idx]);
            if prev_ok && next_ok {
                result.push_str(correct);
                i = next_idx;
            } else {
                result.push(text_chars[i]);
                i += 1;
            }
        } else {
            result.push(text_chars[i]);
            i += 1;
        }
    }
    result
}

/// 判断字符是否为"组词字符"（汉字、字母、数字）——如果是，说明当前匹配可能是更大词的一部分
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || ('\u{4e00}'..='\u{9fff}').contains(&c)
}

// v3: 热词 context biasing — 从 DB 加载热词，拼接成 Qwen3-ASR context 字符串
// P1-3: 场景化动态注入——按会议标题匹配场景，优先取该场景热词，不足补 general
// Qwen3-ASR context 格式: "词1 词2 词3 ..."（空格分隔，最多 30 条，避免拖慢解码）
async fn db_get_hotword_context(db: &crate::db::Database, meeting_id: &str) -> String {
    // 1. 查会议标题推断场景
    let scene: String = sqlx::query("SELECT title FROM meetings WHERE id = ?")
        .bind(meeting_id)
        .fetch_optional(db.pool()).await
        .ok().flatten()
        .map(|r| r.try_get::<String, _>("title").unwrap_or_default())
        .map(|t| infer_scene(&t).to_string())
        .unwrap_or_else(|| "general".to_string());

    // 2. 优先取该场景热词（最多 20 条，按 priority 降序 + 词长降序）
    let scene_rows = sqlx::query(
        "SELECT correct_text FROM hotwords WHERE correct_text IS NOT NULL AND correct_text != '' AND scene = ? ORDER BY priority DESC, length(correct_text) DESC LIMIT 20"
    )
    .bind(&scene)
    .fetch_all(db.pool()).await.unwrap_or_default();

    let mut words: Vec<String> = scene_rows.iter().map(|r| r.get("correct_text")).collect();

    // 3. 不足 30 条时用 general 场景补齐
    if words.len() < 30 {
        let remaining = 30 - words.len();
        let general_rows = sqlx::query(
            "SELECT correct_text FROM hotwords WHERE correct_text IS NOT NULL AND correct_text != '' AND scene = 'general' ORDER BY priority DESC, length(correct_text) DESC LIMIT ?"
        )
        .bind(remaining as i64)
        .fetch_all(db.pool()).await.unwrap_or_default();

        for r in general_rows {
            let w: String = r.get("correct_text");
            if !words.contains(&w) {
                words.push(w);
            }
        }
    }

    // 4. 如果场景热词和 general 加起来仍不足，兜底取全部热词前 30 条
    if words.len() < 10 {
        let all_rows = sqlx::query(
            "SELECT correct_text FROM hotwords WHERE correct_text IS NOT NULL AND correct_text != '' ORDER BY priority DESC, length(correct_text) DESC LIMIT 30"
        )
        .fetch_all(db.pool()).await.unwrap_or_default();

        for r in all_rows {
            let w: String = r.get("correct_text");
            if !words.contains(&w) {
                words.push(w);
            }
        }
    }

    words.join(" ")
}

// URL encode 辅助（Qwen3-ASR context 可能含中文/空格，需编码后放 query string）
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            b' ' => out.push_str("%20"),
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

#[tauri::command]
pub async fn start_recording(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
) -> Result<serde_json::Value, String> {
    eprintln!("[Recording] start_recording 命令被调用, meeting_id={}", meeting_id);

    // 双保险①：录音前确保 ASR 服务在运行（若未运行则启动，最多等 60s）
    // 不在前端判断，后端直接保证——即使前端逻辑再出 bug，录音时也会拉起服务
    if !state.asr_manager.is_running() {
        eprintln!("[Recording] ASR 服务未运行，后台异步拉起（不阻塞录音）...");
        state.asr_manager.clone().start_in_background();
    } else {
        eprintln!("[Recording] ASR 服务已在运行");
    }

    // 停止已有录音（先提取，drop lock 后再 await）
    let prev_task_handle = {
        let mut lock = recording_lock().lock().map_err(|e| e.to_string())?;
        if let Some(rec) = lock.take() {
            eprintln!("[Recording] 停止已有录音");
            rec.cancel_flag.store(true, Ordering::SeqCst);
            // SendStream(cpal) 在此 drop → 自动停止采集
            // 最后一段落盘（同 stop_recording 逻辑）
            drop(rec._stream);
            std::thread::sleep(std::time::Duration::from_millis(150));
            if let Ok(mut guard) = rec.current_segment.lock() {
                if let Some(last) = guard.take() { let _ = last.finish(); }
            }
            rec.task_handle
        } else {
            None
        }
    };
    if let Some(handle) = prev_task_handle {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(30), handle).await;
    }

    // v2.6.0 写入归一：新录音固定写 com.bijian.app（bundle id 正名目录），根治三套目录并存。
    // 存量会议读取/回链走 session_dir 绝对路径（迁移不破坏）；旧目录近匹配逻辑保留（兼容超老数据）。
    let temp_dir = format!("{}/Library/Application Support/com.bijian.app/audio_cache", std::env::var("HOME").unwrap_or_default());
    let session_dir = format!("{}/rec_{}", temp_dir, chrono::Utc::now().timestamp());
    std::fs::create_dir_all(&session_dir).map_err(|e| format!("创建录音目录失败: {}", e))?;
    eprintln!("[Recording] 录音目录: {}", session_dir);

    // 2026-09-04: 会议关联录音目录（录音存档播放用）
    {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query("UPDATE meetings SET session_dir = ?, updated_at = ? WHERE id = ?")
            .bind(&session_dir).bind(&now).bind(&meeting_id)
            .execute(state.db.pool()).await;
    }

    // cpal 原生采集（主进程内 = 麦克风权限直接挂笔尖 bundle，TCC 弹窗稳定）
    let (stream, current_segment, seg_notify) = build_cpal_stream(session_dir.clone())?;
    let pid = 1u32; // 非系统 pid，给前端一个非零标识即可
    eprintln!("[Recording] cpal 原生录音已启动 ({}s/段, 16kHz/I16/单声道)", SEG_DURATION_STR);

    let cancel_flag = Arc::new(AtomicBool::new(false));

    // 克隆资源供后台任务使用
    let http_client = state.http_client.clone();
    let db = state.db.clone();
    let session_dir_clone = session_dir.clone();
    let meeting_id_clone = meeting_id.clone();
    let app_handle_clone = app_handle.clone();
    let cancel_flag_clone = cancel_flag.clone();
    let seg_notify_clone = seg_notify.clone();

    // 启动后台转写任务：段写完 Notify 立即唤醒，500ms 文件扫描兜底
    let task_handle = tauri::async_runtime::spawn(async move {
        eprintln!("[Recording] 后台转写任务启动 (cpal), session_dir={}", session_dir_clone);
        let mut next_seg: u32 = 0;
        // 2026-09-11 防幻觉：连续静音段计数（权限被拒时 cpal 采集全零数据）
        let mut silent_streak: u32 = 0;

        loop {
            // Notify 立即醒（有新段），500ms 兜底扫描（兼容段未写完判断）
            let _ = tokio::time::timeout(
                std::time::Duration::from_millis(500),
                seg_notify_clone.notified(),
            ).await;

            let cancelled = cancel_flag_clone.load(Ordering::SeqCst);

            // 列出所有 segment 文件
            let mut segments: Vec<u32> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&session_dir_clone) {
                for entry in entries.flatten() {
                    let name = entry.file_name();
                    let name_str = name.to_string_lossy();
                    if name_str.starts_with("seg_") && name_str.ends_with(".wav") {
                        if let Some(num_str) = name_str.strip_prefix("seg_").and_then(|s| s.strip_suffix(".wav")) {
                            if let Ok(num) = num_str.parse::<u32>() {
                                segments.push(num);
                            }
                        }
                    }
                }
            }
            segments.sort();

            if segments.is_empty() {
                if cancelled {
                    eprintln!("[Recording] 录音已停止，无段文件");
                    break;
                }
                continue;
            }

            let max_seg = *segments.last().unwrap();

            // 未取消时最后一段可能还在写，不处理；取消后所有段都完整
            let max_processable = if cancelled {
                max_seg
            } else if max_seg > 0 {
                max_seg - 1
            } else {
                continue;
            };

            // 转写 next_seg 到 max_processable 之间所有段
            while next_seg <= max_processable {
                let seg_file = format!("{}/seg_{:03}.wav", session_dir_clone, next_seg);

                if !std::path::Path::new(&seg_file).exists() {
                    next_seg += 1;
                    continue;
                }

                eprintln!("[Recording] 转写段 {} ...", next_seg);
                let start_time = next_seg as f64 * SEG_DURATION_SEC;

                match tokio::fs::read(&seg_file).await {
                    Ok(bytes) => {
                        if bytes.len() < 1000 {
                            eprintln!("[Recording] 段 {} 文件过小 ({} bytes)，跳过", next_seg, bytes.len());
                            next_seg += 1;
                            continue;
                        }

                        // v4: 准实时段转写，0.6B 快速引擎（/qwen3_transcribe），失败重试 3 次不丢段
                        let sherpa_url = std::env::var("BIJIAN_SHERPA_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());
                        let hotword_context = db_get_hotword_context(&db, &meeting_id_clone).await;

                        let mut text = String::new();
                        let mut duration = 0.0f64;
                        let mut engine = "qwen3_asr_quick".to_string();
                        let mut ok = false;
                        let mut is_silent = false;
                        let mut last_err = String::new();
                        // v2.5.0 本地电平检测（一级）：全零段不调 ASR——
                        // 麦克风权限被拒时 cpal 采集全零，落盘即可发现（不等 ASR 的二级判定）
                        if pcm_all_zero(&bytes) {
                            is_silent = true;
                            crate::logger::log("Recording", &format!("段 {} 全零静音（本地电平判定），不调 ASR", next_seg));
                        }
                        for attempt in 1..=3 {
                            if is_silent { break; } // 本地已判静音，不再请求 ASR
                            let form = reqwest::multipart::Form::new()
                                .part("file", reqwest::multipart::Part::bytes(bytes.clone())
                                    .file_name(format!("seg_{:03}.wav", next_seg)));

                            let url = if hotword_context.is_empty() {
                                format!("{}/qwen3_transcribe", sherpa_url)
                            } else {
                                format!("{}/qwen3_transcribe?context={}", sherpa_url, urlencode(&hotword_context))
                            };
                            match http_client.post(&url)
                                .timeout(std::time::Duration::from_secs(120))
                                .multipart(form).send().await
                            {
                                Ok(resp) => {
                                    let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                    text = result.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                                    duration = result.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
                                    // v2.5.2 fallback 可见：记录实际引擎；发生回退的段加 fallback_ 前缀落段级 metadata
                                    let used_fallback = result.get("fallback").and_then(|f| f.as_bool()).unwrap_or(false);
                                    engine = result.get("engine").and_then(|e| e.as_str()).unwrap_or("qwen3_asr_quick").to_string();
                                    if used_fallback { engine = format!("fallback_{}", engine); }
                                    // 2026-09-11 防幻觉：ASR 判定静音段，不重试不入库（静音不是错误）
                                    if result.get("silent").and_then(|s| s.as_bool()).unwrap_or(false) {
                                        is_silent = true;
                                        break;
                                    }
                                    if !text.is_empty() && text != "<sil>" {
                                        ok = true;
                                        break;
                                    }
                                    last_err = "空文本或静音".to_string();
                                }
                                Err(e) => {
                                    last_err = e.to_string();
                                    eprintln!("[Recording] 段 {} 第{}次ASR请求失败: {}", next_seg, attempt, e);
                                }
                            }
                            // 指数退避重试：1s / 2s
                            tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;
                        }

                        if ok {
                            // P5-5: 热词后处理纠正
                            let text = db_apply_hotwords(&text, &db).await;

                            let cid = Uuid::new_v4().to_string();
                            let _ = sqlx::query(
                                "INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count) VALUES (?, ?, ?, ?, ?, 'unknown', 0.9, ?)"
                            )
                            .bind(&cid).bind(&meeting_id_clone)
                            .bind(start_time).bind(start_time + duration)
                            .bind(&text).bind(text.len() as i64)
                            .execute(db.pool()).await;

                            // 创建 chunk_summary job
                            let jid = Uuid::new_v4().to_string();
                            let _ = sqlx::query(
                                "INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'chunk_summary', 'pending', ?)"
                            )
                            .bind(&jid).bind(&meeting_id_clone)
                            .bind(serde_json::json!({"chunk_id": cid}).to_string())
                            .execute(db.pool()).await;

                            // v2.5.0 状态机改真：段成功不再直接标 transcribed（状态=事实）
                            upsert_segment_state(&db, &meeting_id_clone, next_seg, "done", "").await;
                            // v2.5.2 fallback 可见：段级引擎标识落库（fallback_qwen3_asr_quick = 本段经历过引擎回退）
                            set_segment_engine(&db, &meeting_id_clone, next_seg, &engine).await;
                            update_meeting_status_by_segments(&db, &app_handle_clone, &meeting_id_clone, false).await;

                            // 推送事件到前端
                            let _ = app_handle_clone.emit("transcript_chunk", serde_json::json!({
                                "text": text,
                                "start_ts": start_time,
                                "speaker": "unknown",
                                "engine": engine,
                                "chunk_id": cid,
                                "segment_index": next_seg,
                            }));

                            let preview: String = text.chars().take(50).collect();
                            crate::logger::log("Recording", &format!("段 {} 转写完成: {}", next_seg, preview));
                        } else if is_silent {
                            // 二级静音（ASR VAD 判定）
                        } else {
                            // 3次重试都失败：不丢段，段行落 failed（差集补跑/巡检可重试）
                            upsert_segment_state(&db, &meeting_id_clone, next_seg, "failed", &last_err).await;
                            update_meeting_status_by_segments(&db, &app_handle_clone, &meeting_id_clone, false).await;
                            crate::logger::log("Recording", &format!("段 {} 转写 3 次失败: {}", next_seg, last_err));
                            let _ = app_handle_clone.emit("transcript_chunk", serde_json::json!({
                                "text": "（本段转写失败，精转后将补全）",
                                "start_ts": start_time,
                                "speaker": "unknown",
                                "engine": "qwen3_asr_quick",
                                "chunk_id": Uuid::new_v4().to_string(),
                                "segment_index": next_seg,
                            }));
                        }

                        // 2026-09-11 防幻觉：静音段处理——不入库（避免 "15%" 幻觉文本污染转写），
                        // 只计数；连续 ≥3 段静音推 mic_permission 警告事件（典型原因：
                        // macOS 麦克风权限被拒后 cpal 静默采集全零数据）
                        if is_silent && !ok {
                            silent_streak += 1;
                            upsert_segment_state(&db, &meeting_id_clone, next_seg, "silent", "静音段不入库").await;
                            update_meeting_status_by_segments(&db, &app_handle_clone, &meeting_id_clone, false).await;
                            let _ = app_handle_clone.emit("transcript_chunk", serde_json::json!({
                                "text": "",
                                "start_ts": start_time,
                                "speaker": "unknown",
                                "engine": "qwen3_asr_quick",
                                "silent": true,
                                "silent_streak": silent_streak,
                                "chunk_id": Uuid::new_v4().to_string(),
                                "segment_index": next_seg,
                            }));
                            // v2.5.0: 30 秒（3 段）→ 20 秒（2 段）强警告——权限问题早 10 秒暴露
                            if silent_streak >= 2 {
                                crate::logger::log("Recording", &format!("连续 {} 段静音，疑似麦克风权限问题，推送警告", silent_streak));
                                let _ = app_handle_clone.emit("mic_permission_warning", serde_json::json!({
                                    "consecutive_silent": silent_streak,
                                    "message": "已连续多段采集到无声数据，可能是 macOS 麦克风权限未授予。请到 系统设置 → 隐私与安全性 → 麦克风 中允许笔尖，然后重启应用。",
                                }));
                            }
                        } else {
                            silent_streak = 0;
                        }
                    }
                    Err(e) => {
                        eprintln!("[Recording] 段 {} 读取失败: {}", next_seg, e);
                    }
                }

                next_seg += 1;
            }

            // === 滚动精转已移除（2026-09-04）===
            // 0.6B 精转 RTF≈1.4 太慢（55分钟录音需77分钟），用户不可接受。
            // 纪要直接使用快转段（conf=0.9）生成，说话人分离走 num_clusters=2 diarization。
            // 停止录音后由前端直接调用 run_speaker_diarization（不再走 run_cross_validate_transcribe）。

            if cancelled && next_seg > max_seg {
                eprintln!("[Recording] 所有段已转写完成");
                break;
            }
        }

        eprintln!("[Recording] 后台转写任务结束");
    });

    {
        let mut lock = recording_lock().lock().map_err(|e| e.to_string())?;
        *lock = Some(ActiveRecording {
            _stream: SendStream(stream),
            current_segment,
            seg_notify,
            session_dir: session_dir.clone(),
            meeting_id: meeting_id.clone(),
            cancel_flag,
            task_handle: Some(task_handle),
        });
    }

    eprintln!("[Recording] 录音已开始, session_dir={}", session_dir);
    Ok(serde_json::json!({
        "recording": true,
        "session_dir": session_dir,
        "pid": pid
    }))
}

#[tauri::command]
pub async fn stop_recording(
    app_handle: AppHandle,
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    eprintln!("[Recording] stop_recording 命令被调用");

    let (rec_state, cancel_flag, task_handle, session_dir, meeting_id) = {
        let mut lock = recording_lock().lock().map_err(|e| e.to_string())?;
        match lock.take() {
            Some(rec) => {
                let rs = (rec._stream, rec.current_segment, rec.seg_notify);
                (rs, rec.cancel_flag, rec.task_handle, rec.session_dir, rec.meeting_id)
            }
            None => {
                eprintln!("[Recording] 无正在运行的录音进程");
                return Ok(serde_json::json!({
                    "recording": false,
                    "segment_count": 0
                }));
            }
        }
    };

    let (_stream, current_segment, _seg_notify) = rec_state;
    // 设置取消标志，通知后台任务可以处理最后一段了
    cancel_flag.store(true, Ordering::SeqCst);
    eprintln!("[Recording] 已设置取消标志");

    // cpal Stream drop 时自动暂停；手动把最后一段（可能未满）落盘 → 后台能扫描到
    // 注意：cpal 回调此时可能还在写入 seg_arc，先 pause 再加锁
    // cpal::Stream 没有显式 pause 但 Drop 会自动停止。我们通过 drop 引用让 RAII 停：
    drop(_stream);
    // 给音频回调最后一次进入（~100ms 足够）
    std::thread::sleep(std::time::Duration::from_millis(150));
    // finish 最后一个段文件
    if let Ok(mut guard) = current_segment.lock() {
        if let Some(last) = guard.take() {
            match last.finish() {
                Ok(idx) => eprintln!("[Recording] 最后段 {} 落盘完成", idx),
                Err(e) => eprintln!("[Recording] 最后段落盘失败: {}", e),
            }
        }
    }

    // 等待后台转写任务完成（最多 120 秒，处理最后一段）
    // 2026-09-03: 超时不再丢任务——任务仍在跑就 abort，spawn 独立补跑任务继续转剩余段
    let mut timed_out = false;
    if let Some(handle) = task_handle {
        eprintln!("[Recording] 等待后台转写任务完成...");
        match tokio::time::timeout(std::time::Duration::from_secs(120), handle).await {
            Ok(_) => eprintln!("[Recording] 后台转写任务已完成"),
            Err(_) => {
                timed_out = true;
                eprintln!("[Recording] 后台转写任务超时（120s），转后台补跑");
            }
        }
    }

    // 统计录音段数
    let segment_count = std::fs::read_dir(&session_dir)
        .map(|entries| entries.flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with("seg_"))
            .count())
        .unwrap_or(0);

    // v2.5.0: 录音停止——写入 expected_segments（状态机事实基准），status→transcribing
    {
        let now = chrono::Utc::now().to_rfc3339();
        let _ = sqlx::query(
            "UPDATE meetings SET expected_segments = ?, status = CASE WHEN status='recording' THEN 'transcribing' ELSE status END, updated_at = ? WHERE id = ?"
        )
        .bind(segment_count as i64).bind(&now).bind(&meeting_id)
        .execute(state.db.pool()).await;
    }
    crate::logger::log("Recording", &format!("录音结束, segments={}, timed_out={}", segment_count, timed_out));

    // v2.5.0 差集补跑：不再仅在超时后补——只要“段文件/chunks 差集非空”就补
    // （根治：实时循环提前退出/被杀时不超时也不补的漏洞；9-25 事故直接对应）
    {
        let sd = session_dir.clone();
        let mid = meeting_id.clone();
        let db = state.db.clone();
        let client = state.http_client.clone();
        let ah = app_handle.clone();
        tauri::async_runtime::spawn(async move {
            let missing = diff_missing_segments(&db, &mid, &sd).await;
            if missing.is_empty() {
                // 无差集：全部段已终态，做一次状态机收敛（终态判定）
                update_meeting_status_by_segments(&db, &ah, &mid, true).await;
                crate::logger::log("Recording", &format!("会议 {} 停止后无差集，状态收敛完成", &mid.chars().take(8).collect::<String>()));
            } else {
                crate::logger::log("Recording", &format!("会议 {} 停止后差集 {} 段，启动补跑", &mid.chars().take(8).collect::<String>(), missing.len()));
                complete_transcribe(&sd, &mid, &db, &client, &ah).await;
            }
        });
    }

    // 返回 session_dir，前端收到后调用 run_speaker_diarization
    Ok(serde_json::json!({
        "recording": false,
        "session_dir": session_dir,
        "segment_count": segment_count
    }))
}

/// ===== v2.5.0 可靠性工程：段级持久化 + 状态机 + 差集工具 =====

/// 检测 PCM 音频是否全零（跳过 44 字节 WAV 头）——不依赖 ASR 的本地电平判定，
/// 麦克风权限被拒时 cpal 采集全零数据（9-24 事故根因），落盘后立即发现
fn pcm_all_zero(bytes: &[u8]) -> bool {
    if bytes.len() < 44 + 2 { return true; }
    bytes[44..].iter().all(|&b| b == 0)
}

/// 段文件名扫描：返回有序段号列表
fn scan_segment_files(session_dir: &str) -> Vec<u32> {
    let mut segments: Vec<u32> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(num_str) = name_str.strip_prefix("seg_").and_then(|s| s.strip_suffix(".wav")) {
                if let Ok(num) = num_str.parse::<u32>() {
                    segments.push(num);
                }
            }
        }
    }
    segments.sort();
    segments
}

/// 段终态落库（upsert）——done/silent/failed；pending 行由差集扫描创建
#[doc(hidden)]
pub async fn upsert_segment_state(db: &crate::db::Database, meeting_id: &str, seg: u32, state: &str, last_error: &str) {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    let _ = sqlx::query(
        "INSERT INTO transcription_segments (id, meeting_id, segment_index, state, attempts, last_error, started_at, finished_at)\n         VALUES (?, ?, ?, ?, 1, ?, '', ?)\n         ON CONFLICT(meeting_id, segment_index) DO UPDATE SET state=excluded.state, last_error=excluded.last_error, finished_at=excluded.finished_at"
    )
    .bind(Uuid::new_v4().to_string()).bind(meeting_id).bind(seg as i64)
    .bind(state).bind(last_error).bind(&now)
    .execute(db.pool()).await;
}

/// v2.5.2 fallback 可见：落段级实际引擎标识。
/// 发生引擎回退的段记 "fallback_<engine>"（Python 侧 requested=paraformer 但实际用 0.6B），
/// 会议级 fallback_count 由收敛函数从段级表直接统计——引擎退化不再静默。
async fn set_segment_engine(db: &crate::db::Database, meeting_id: &str, seg: u32, engine: &str) {
    let _ = sqlx::query("UPDATE transcription_segments SET engine = ? WHERE meeting_id = ? AND segment_index = ?")
        .bind(engine).bind(meeting_id).bind(seg as i64)
        .execute(db.pool()).await;
}

/// 差集扫描：段文件存在但无终态行且无 chunk 的段号集合（需要补转的段）
/// 兼容旧数据：历史 chunks 无段行时视为已转（补一行 done）
#[doc(hidden)]
pub async fn diff_missing_segments(db: &crate::db::Database, meeting_id: &str, session_dir: &str) -> Vec<u32> {
    let files = scan_segment_files(session_dir);
    if files.is_empty() { return vec![]; }
    let rows: Vec<(i64, String, String)> = sqlx::query(
        "SELECT segment_index, state, started_at FROM transcription_segments WHERE meeting_id = ?"
    )
    .bind(meeting_id)
    .fetch_all(db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<i64,_>("segment_index"), r.get::<String,_>("state"), r.get::<String,_>("started_at"))).collect())
    .unwrap_or_default();
    let mut finished: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut busy: std::collections::HashSet<u32> = std::collections::HashSet::new(); // 近期 processing（租约）
    for (idx, st, started) in &rows {
        let idx = *idx as u32;
        match st.as_str() {
            "done" | "silent" | "failed" => { finished.insert(idx, st.clone()); }
            "processing" => {
                let fresh = chrono::NaiveDateTime::parse_from_str(started, "%Y-%m-%d %H:%M:%S")
                    .map(|t| (chrono::Utc::now().naive_utc() - t).num_minutes() < 10).unwrap_or(false);
                if fresh { busy.insert(idx); } // 其他 worker 正在处理，不碰（简单租约）
            }
            _ => {} // pending → 差集候选
        }
    }
    // 已有 chunk 的段（旧数据兼容）：补 done 行
    let chunk_starts: Vec<f64> = sqlx::query("SELECT start_time FROM chunks WHERE meeting_id = ?")
        .bind(meeting_id)
        .fetch_all(db.pool()).await
        .map(|rs| rs.iter().map(|r| r.get::<f64,_>("start_time")).collect())
        .unwrap_or_default();
    for st in &chunk_starts {
        let seg = (st / SEG_DURATION_SEC).floor() as u32;
        if !finished.contains_key(&seg) {
            upsert_segment_state(db, meeting_id, seg, "done", "").await;
            finished.insert(seg, "done".to_string());
        }
    }
    files.into_iter().filter(|f| !finished.contains_key(f) && !busy.contains(f)).collect()
}

/// 状态机改真：状态必须等于事实（9-25 修复“1/8 完成标 transcribed”的失真）
/// v2.5.1 升级为唯一统计收敛函数：expected/completed/failed 三项全部以段级表为事实基准回写，
/// 并加段级 0 行保护（导入型会议/老会议无段级事实，不据此改状态，防误标 failed）。
#[doc(hidden)]
pub async fn update_meeting_status_by_segments<R: tauri::Runtime>(db: &crate::db::Database, app_handle: &tauri::AppHandle<R>, meeting_id: &str, recording_stopped: bool) {
    let row = sqlx::query(
        "SELECT\n            COALESCE(SUM(CASE WHEN state='done' THEN 1 ELSE 0 END),0) as done,\n            COALESCE(SUM(CASE WHEN state='silent' THEN 1 ELSE 0 END),0) as silent,\n            COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0) as failed,\n            COALESCE(SUM(CASE WHEN state IN ('pending','processing') THEN 1 ELSE 0 END),0) as unfinished,\n            COUNT(*) as total\n        FROM transcription_segments WHERE meeting_id = ?"
    )
    .bind(meeting_id)
    .fetch_optional(db.pool()).await
    .ok().flatten();
    let (done, silent, failed, unfinished, total): (i64, i64, i64, i64, i64) = match row {
        Some(r) => (r.get::<i64,_>("done"), r.get::<i64,_>("silent"), r.get::<i64,_>("failed"), r.get::<i64,_>("unfinished"), r.get::<i64,_>("total")),
        None => (0, 0, 0, 0, 0),
    };
    // v2.5.1 段级 0 行保护：无段级事实（音频导入路径 / v2.5.0 前老会议）不收敛，
    // 否则会把导入完成的 transcribed 误标为 failed
    if total == 0 {
        return;
    }
    let now = chrono::Utc::now().to_rfc3339();
    if !recording_stopped {
        if done + silent + failed > 0 {
            let _ = sqlx::query("UPDATE meetings SET status='transcribing', expected_segments=?, completed_segments=?, failed_segments=?, updated_at=? WHERE id=? AND status IN ('recording','pending')")
                .bind(total).bind(done).bind(failed).bind(&now).bind(meeting_id)
                .execute(db.pool()).await;
        }
        return;
    }
    let new_status: &str = if unfinished > 0 {
        "transcribing"
    } else if done > 0 {
        if failed > 0 { "transcription_partial" } else { "transcribed" }
    } else if silent > 0 {
        "no_voice"
    } else {
        "failed"
    };
    let _ = sqlx::query("UPDATE meetings SET status=?, expected_segments=?, completed_segments=?, failed_segments=?, updated_at=? WHERE id=?")
        .bind(new_status).bind(total).bind(done).bind(failed).bind(&now).bind(meeting_id)
        .execute(db.pool()).await;
    let short_id: String = meeting_id.chars().take(8).collect();
    crate::logger::log("StatusMachine", &format!("会议 {} → {} (done={} silent={} failed={} unfinished={} total={})", short_id, new_status, done, silent, failed, unfinished, total));
    // v2.5.2 fallback 可见：会议级回退段数（engine LIKE 'fallback%' 的 done 段），前端提示"部分片段使用备用引擎"
    let fallback_count: i64 = sqlx::query(
        "SELECT COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? AND state = 'done' AND engine LIKE 'fallback%'"
    )
    .bind(meeting_id)
    .fetch_optional(db.pool()).await
    .ok().flatten()
    .map(|r| r.get::<i64, _>("c"))
    .unwrap_or(0);
    let _ = app_handle.emit("meeting_status_changed", serde_json::json!({
        "meeting_id": meeting_id, "status": new_status,
        "done": done, "silent": silent, "failed": failed, "unfinished": unfinished,
        "fallback_count": fallback_count,
    }));
}

/// v2.5.1 启动一致性巡检：段级表是唯一事实来源，主表统计失真自动收敛。
/// 幂等——重复执行结果一致；不触碰 recording 中的会议（录音状态由录音循环管理）。
pub async fn consistency_check_and_repair<R: tauri::Runtime>(db: &crate::db::Database, app_handle: &tauri::AppHandle<R>) {
    let repaired = sqlx::query(
        "UPDATE meetings SET\n            expected_segments = (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id),\n            completed_segments = (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id AND t.state = 'done'),\n            failed_segments = (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id AND t.state = 'failed'),\n            updated_at = updated_at\n        WHERE EXISTS (SELECT 1 FROM transcription_segments t WHERE t.meeting_id = meetings.id)\n          AND (expected_segments != (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id)\n            OR completed_segments != (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id AND t.state = 'done')\n            OR failed_segments != (SELECT COUNT(*) FROM transcription_segments t WHERE t.meeting_id = meetings.id AND t.state = 'failed'))"
    )
    .execute(db.pool()).await
    .map(|r| r.rows_affected())
    .unwrap_or(0);
    if repaired > 0 {
        crate::logger::log("ConsistencyCheck", &format!("统计字段失真收敛：{} 场会议的 expected/completed/failed 已按段级表回填", repaired));
    } else {
        crate::logger::log("ConsistencyCheck", "统计字段与段级表一致，无需修复");
    }
    // 失真状态修复：段级事实与 status 冲突的非录音会议，重新走收敛（transcribed/partial/no_voice/failed）
    let stale: Vec<String> = sqlx::query(
        "SELECT DISTINCT m.id FROM meetings m JOIN transcription_segments t ON t.meeting_id = m.id\n        WHERE m.status NOT IN ('recording')\n          AND (\n            (m.status = 'transcribed'  AND (SELECT COUNT(*) FROM transcription_segments x WHERE x.meeting_id = m.id AND x.state IN ('pending','processing','failed')) > 0)\n         OR (m.status IN ('transcribing','pending') AND (SELECT COUNT(*) FROM transcription_segments x WHERE x.meeting_id = m.id AND x.state IN ('pending','processing')) = 0)\n         OR (m.status = 'no_voice'      AND (SELECT COUNT(*) FROM transcription_segments x WHERE x.meeting_id = m.id AND x.state = 'done') > 0)\n          )"
    )
    .fetch_all(db.pool()).await
    .map(|rs| rs.iter().map(|r| r.get::<String,_>("id")).collect())
    .unwrap_or_default();
    for mid in stale {
        // completed 是纪要链路的终态推进，巡检不回退（transcribed→completed 合法），只修失真
        crate::logger::log("ConsistencyCheck", &format!("会议 {} 状态与段级事实冲突，执行收敛", &mid[..mid.len().min(8)]));
        update_meeting_status_by_segments(db, app_handle, &mid, true).await;
    }
}

/// 启动续转 / 定时巡检：扫描所有未完成会议的差集，非空则补转
/// 覆盖场景：app 升级替换、崩溃、OOM 被杀——进程死了任务不丢（9-25 核心修复）
pub async fn resume_unfinished_transcriptions(db: std::sync::Arc<crate::db::Database>, http_client: reqwest::Client, app_handle: &tauri::AppHandle) {
    let meetings: Vec<(String, String)> = sqlx::query(
        "SELECT id, session_dir FROM meetings WHERE session_dir != '' AND status NOT IN ('completed','no_voice','failed','recording')"
    )
    .fetch_all(db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<String,_>("id"), r.get::<String,_>("session_dir"))).collect())
    .unwrap_or_default();
    for (mid, sd) in meetings {
        let missing = diff_missing_segments(&db, &mid, &sd).await;
        if missing.is_empty() { continue; }
        let short_id: String = mid.chars().take(8).collect();
        crate::logger::log("Resume", &format!("会议 {} 发现 {} 段未转，启动续转", short_id, missing.len()));
        // Arc/Client clone 廉价，可逃逸到 spawn
        let db_owned = db.clone();
        let http_owned = http_client.clone();
        let ah = app_handle.clone();
        let sd2 = sd.clone(); let mid2 = mid.clone();
        tauri::async_runtime::spawn(async move {
            complete_transcribe(&sd2, &mid2, &db_owned, &http_owned, &ah).await;
        });
    }
}

/// 补跑：以段级持久化为准补齐缺失段（stop 超时/进程被杀/启动恢复的统一兄底）
/// 不重复转已入库段（避免与实时任务重复写库）
async fn complete_transcribe<R: tauri::Runtime>(
    session_dir: &str,
    meeting_id: &str,
    db: &crate::db::Database,
    http_client: &reqwest::Client,
    app_handle: &tauri::AppHandle<R>,
) {
    // 1. 差集扫描：需要补转的段（含旧数据兼容与 worker 租约）
    let missing = diff_missing_segments(db, meeting_id, session_dir).await;
    if missing.is_empty() {
        // 无差集也做一次状态机收敛（修复旧版状态失真，如已标 transcribed 但实际未转完）
        update_meeting_status_by_segments(db, app_handle, meeting_id, true).await;
        return;
    }
    let short_id: String = meeting_id.chars().take(8).collect();
    crate::logger::log("Backfill", &format!("会议 {} 差集 {} 段待补转", short_id, missing.len()));

    // 2. 逐段补转
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());
    let hotword_context = db_get_hotword_context(db, meeting_id).await;

    for next_seg in missing {
        let seg_file = format!("{}/seg_{:03}.wav", session_dir, next_seg);
        let bytes = match std::fs::read(&seg_file) {
            Ok(b) => b,
            Err(e) => {
                upsert_segment_state(db, meeting_id, next_seg, "failed", &format!("读文件失败: {}", e)).await;
                update_meeting_status_by_segments(db, app_handle, meeting_id, true).await;
                continue;
            }
        };
        if bytes.len() < 1000 {
            upsert_segment_state(db, meeting_id, next_seg, "silent", "段文件过小").await;
            update_meeting_status_by_segments(db, app_handle, meeting_id, true).await;
            continue;
        }

        // 建行 + 领取（简单租约）：
        // 1) INSERT OR IGNORE 确保段行存在（差集段无行时补建 pending）
        // 2) 只抢 pending 或 stale processing（started_at 超 10 分钟，原 worker 已死如 app 被杀）
        let now_str = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        let cutoff = (chrono::Utc::now() - chrono::Duration::minutes(10)).format("%Y-%m-%d %H:%M:%S").to_string();
        let _ = sqlx::query(
            "INSERT OR IGNORE INTO transcription_segments (id, meeting_id, segment_index, state, attempts, last_error, started_at, finished_at) VALUES (?, ?, ?, 'pending', 0, '', '', '')"
        )
        .bind(Uuid::new_v4().to_string()).bind(meeting_id).bind(next_seg as i64)
        .execute(db.pool()).await;
        let claimed = sqlx::query(
            "UPDATE transcription_segments SET state='processing', started_at=? WHERE meeting_id=? AND segment_index=? AND (state='pending' OR (state='processing' AND (started_at='' OR started_at < ?)))"
        )
        .bind(&now_str).bind(meeting_id).bind(next_seg as i64).bind(&cutoff)
        .execute(db.pool()).await.map(|r| r.rows_affected() > 0).unwrap_or(false);
        if !claimed {
            crate::logger::log("Backfill", &format!("段 {} 领取失败（被其他 worker 持有），跳过", next_seg));
            continue;
        }

        let start_time = next_seg as f64 * SEG_DURATION_SEC;
        let mut text = String::new();
        let mut duration = 0.0f64;
        let mut ok = false;
        let mut silent = false;
        let mut last_err = String::new();
        let mut seg_engine = String::new(); // v2.5.2 fallback 可见

        // 本地电平检测：全零段不调 ASR（防幻觉 + 省 58 秒/段的算力）
        if pcm_all_zero(&bytes) {
            silent = true;
            crate::logger::log("Backfill", &format!("段 {} 全零静音（本地电平判定），不入库", next_seg));
        } else {
            for attempt in 1..=3 {
                let form = reqwest::multipart::Form::new()
                    .part("file", reqwest::multipart::Part::bytes(bytes.clone())
                        .file_name(format!("seg_{:03}.wav", next_seg)));
                let url = if hotword_context.is_empty() {
                    format!("{}/qwen3_transcribe", asr_url)
                } else {
                    format!("{}/qwen3_transcribe?context={}", asr_url, urlencode(&hotword_context))
                };
                match http_client.post(&url)
                    .timeout(std::time::Duration::from_secs(120))
                    .multipart(form).send().await
                {
                    Ok(resp) => {
                        let result: serde_json::Value = resp.json().await.unwrap_or_default();
                        text = result.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
                        duration = result.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
                        // v2.5.2 fallback 可见：补转段同样记录实际引擎（含回退标记）
                        let used_fallback = result.get("fallback").and_then(|f| f.as_bool()).unwrap_or(false);
                        seg_engine = result.get("engine").and_then(|e| e.as_str()).unwrap_or("qwen3_asr_quick").to_string();
                        if used_fallback { seg_engine = format!("fallback_{}", seg_engine); }
                        // 静音判定（ASR 端 VAD/防幻觉二级判定）
                        if result.get("silent").and_then(|s| s.as_bool()).unwrap_or(false) {
                            silent = true;
                            break;
                        }
                        if !text.is_empty() && text != "<sil>" {
                            ok = true;
                            break;
                        }
                        last_err = "空文本或静音".to_string();
                    }
                    Err(e) => {
                        last_err = e.to_string();
                        crate::logger::log("Backfill", &format!("段 {} 第{}次请求失败: {}", next_seg, attempt, e));
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(attempt as u64)).await;
            }
        }

        if ok {
            let text = db_apply_hotwords(&text, db).await;
            let cid = Uuid::new_v4().to_string();
            let _ = sqlx::query(
                "INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count) VALUES (?, ?, ?, ?, ?, 'unknown', 0.9, ?)"
            )
            .bind(&cid).bind(meeting_id)
            .bind(start_time).bind(start_time + duration)
            .bind(&text).bind(text.len() as i64)
            .execute(db.pool()).await;

            let jid = Uuid::new_v4().to_string();
            let _ = sqlx::query(
                "INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'chunk_summary', 'pending', ?)"
            )
            .bind(&jid).bind(meeting_id)
            .bind(serde_json::json!({"chunk_id": cid}).to_string())
            .execute(db.pool()).await;

            let _ = app_handle.emit("transcript_chunk", serde_json::json!({
                "text": text, "start_ts": start_time, "speaker": "unknown",
                "engine": "qwen3_asr_quick", "chunk_id": cid, "segment_index": next_seg,
            }));
            upsert_segment_state(db, meeting_id, next_seg, "done", "").await;
            // v2.5.2 fallback 可见：补转段引擎标识落库
            if !seg_engine.is_empty() { set_segment_engine(db, meeting_id, next_seg, &seg_engine).await; }
            crate::logger::log("Backfill", &format!("段 {} 补转完成: {}", next_seg, text.chars().take(30).collect::<String>()));
        } else if silent {
            upsert_segment_state(db, meeting_id, next_seg, "silent", "静音段不入库").await;
        } else {
            upsert_segment_state(db, meeting_id, next_seg, "failed", &last_err).await;
            crate::logger::log("Backfill", &format!("段 {} 补转失败（3次重试）: {}", next_seg, last_err));
        }
        // 每段终态后状态机收敛
        update_meeting_status_by_segments(db, app_handle, meeting_id, true).await;
    }
    let _ = app_handle.emit("transcript_done", serde_json::json!({
        "meeting_id": meeting_id, "complete": true,
    }));
    crate::logger::log("Backfill", &format!("会议 {} 补跑结束", short_id));
}

#[tauri::command]
pub async fn get_recording_status() -> Result<RecordingStatus, String> {
    let lock = recording_lock().lock().map_err(|e| e.to_string())?;
    Ok(RecordingStatus {
        recording: lock.is_some(),
        file_path: lock.as_ref().map(|r| r.session_dir.clone()),
        duration: 0.0,
    })
}

// ========== 说话人分离 (P2) ==========

/// 将多个 WAV 段拼接成一个完整 WAV 文件
fn concat_wav_segments(session_dir: &str) -> Result<String, String> {
    let mut segments: Vec<u32> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(session_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("seg_") && name_str.ends_with(".wav") {
                if let Some(num_str) = name_str.strip_prefix("seg_").and_then(|s| s.strip_suffix(".wav")) {
                    if let Ok(num) = num_str.parse::<u32>() {
                        segments.push(num);
                    }
                }
            }
        }
    }
    segments.sort();

    if segments.is_empty() {
        return Err("没有录音段文件".to_string());
    }

    // 读取第一个段获取 WAV 参数
    let first_file = format!("{}/seg_{:03}.wav", session_dir, segments[0]);
    let first_bytes = std::fs::read(&first_file).map_err(|e| format!("读取段文件失败: {}", e))?;

    // 正确解析 WAV 结构：找到 data chunk 的起始位置
    // WAV 格式: RIFF<size>WAVE<fmt chunk><可选其他chunk如LIST>data<size><音频数据>
    fn find_data_chunk_offset(bytes: &[u8]) -> Option<(usize, usize)> {
        // 返回 (data_offset, data_len)
        if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
            return None;
        }
        let mut pos = 12; // 跳过 RIFF header
        while pos + 8 <= bytes.len() {
            let chunk_id = &bytes[pos..pos+4];
            let chunk_size = u32::from_le_bytes([bytes[pos+4], bytes[pos+5], bytes[pos+6], bytes[pos+7]]) as usize;
            if chunk_id == b"data" {
                return Some((pos + 8, chunk_size));
            }
            // chunk 大小奇数要 padding 对齐
            pos += 8 + chunk_size + (chunk_size % 2);
        }
        None
    }

    let (data_off, _) = find_data_chunk_offset(&first_bytes)
        .ok_or("无法解析 WAV data chunk 位置")?;
    eprintln!("[Diarization] WAV data chunk 偏移: {} 字节", data_off);

    // 构建标准 44 字节 WAV 头（去掉 LIST/INFO 等非必要 chunk）
    let mut output = Vec::with_capacity(44 + segments.len() * 960000);
    // RIFF header
    output.extend_from_slice(b"RIFF");
    // RIFF size = 36 + data_size (先占位，后面填)
    output.extend_from_slice(&[0u8; 4]);
    output.extend_from_slice(b"WAVE");
    // fmt chunk
    output.extend_from_slice(b"fmt ");
    output.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size = 16
    output.extend_from_slice(&1u16.to_le_bytes());  // PCM
    output.extend_from_slice(&1u16.to_le_bytes());  // mono
    output.extend_from_slice(&16000u32.to_le_bytes()); // sample rate
    output.extend_from_slice(&32000u32.to_le_bytes()); // byte rate = 16000*2*1
    output.extend_from_slice(&2u16.to_le_bytes());  // block align
    output.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    // data chunk header (先占位 size)
    output.extend_from_slice(b"data");
    output.extend_from_slice(&[0u8; 4]); // data size 占位

    // 拼接所有段的音频数据（跳过各自文件头）
    let mut all_data = Vec::new();
    for seg_num in &segments {
        let seg_file = format!("{}/seg_{:03}.wav", session_dir, seg_num);
        if let Ok(bytes) = std::fs::read(&seg_file) {
            if let Some((off, _)) = find_data_chunk_offset(&bytes) {
                all_data.extend_from_slice(&bytes[off..]);
            }
        }
    }

    let data_size = all_data.len() as u32;
    // 回填 data chunk size (offset 40-43)
    let size_bytes = data_size.to_le_bytes();
    output[40] = size_bytes[0];
    output[41] = size_bytes[1];
    output[42] = size_bytes[2];
    output[43] = size_bytes[3];
    // 回填 RIFF chunk size (offset 4-7) = data_size + 36
    let riff_size = (data_size + 36) as u32;
    let riff_bytes = riff_size.to_le_bytes();
    output[4] = riff_bytes[0];
    output[5] = riff_bytes[1];
    output[6] = riff_bytes[2];
    output[7] = riff_bytes[3];

    output.extend_from_slice(&all_data);

    let concat_path = format!("{}/full_audio.wav", session_dir);
    std::fs::write(&concat_path, &output).map_err(|e| format!("写入拼接文件失败: {}", e))?;
    eprintln!("[Diarization] 拼接 WAV 完成: {} ({} bytes, {} 段)", concat_path, output.len(), segments.len());

    Ok(concat_path)
}

/// 说话人分离：对完整录音运行 diarization，将 speaker 标签写回 chunks 表
#[tauri::command]
pub async fn run_speaker_diarization(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    session_dir: String,
) -> Result<serde_json::Value, String> {
    if !crate::feature::is_feature_enabled(crate::feature::FeatureFlag::SpeakerDiarization) {
        return Err("说话人分离功能需要 Pro 版".to_string());
    }
    eprintln!("[Diarization] 开始说话人分离, meeting_id={}", meeting_id);

    // 1. 拼接 WAV
    let concat_path = concat_wav_segments(&session_dir)?;

    // 2. 调用 Sherpa 服务的 /diarize 端点（只在 :8083 上）
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());
    eprintln!("[Diarization] 调用 {} /diarize ...", asr_url);

    let wav_bytes = tokio::fs::read(&concat_path).await
        .map_err(|e| format!("读取拼接文件失败: {}", e))?;

    let form = reqwest::multipart::Form::new()
        .part("file", reqwest::multipart::Part::bytes(wav_bytes)
            .file_name("full_audio.wav"));

    let resp = state.http_client.post(format!("{}/diarize", asr_url))
        .multipart(form).send().await
        .map_err(|e| format!("diarize 请求失败: {}", e))?;

    let result: serde_json::Value = resp.json().await
        .map_err(|e| format!("diarize 响应解析失败: {}", e))?;

    eprintln!("[Diarization] 响应: {}", serde_json::to_string(&result).unwrap_or_default());

    let segments = result.get("segments").and_then(|s| s.as_array()).cloned().unwrap_or_default();
    let num_speakers = result.get("num_speakers").and_then(|n| n.as_u64()).unwrap_or(0);

    if segments.is_empty() {
        eprintln!("[Diarization] 未检测到说话人段");
        return Ok(serde_json::json!({
            "success": true,
            "num_speakers": 0,
            "assigned_chunks": 0,
            "message": "未检测到多个说话人"
        }));
    }

    // 3. 从 DB 读取该会议的所有 chunks
    let chunks: Vec<(String, f64, f64)> = sqlx::query(
        "SELECT id, start_time, end_time FROM chunks WHERE meeting_id = ? ORDER BY start_time"
    )
    .bind(&meeting_id)
    .fetch_all(state.db.pool()).await
    .map_err(|e| format!("查询 chunks 失败: {}", e))?
    .iter()
    .map(|r| (r.get("id"), r.get("start_time"), r.get("end_time")))
    .collect();

    eprintln!("[Diarization] 共 {} 个 chunks, {} 个说话人段", chunks.len(), segments.len());

    // 4. 把每个 chunk 的时间区间映射到说话人段
    // 策略：找到与 chunk 时间区间重叠最多的说话人段
    let mut assigned = 0u32;
    for (chunk_id, chunk_start, chunk_end) in &chunks {
        let cs = *chunk_start;
        let ce = *chunk_end;
        let chunk_mid = (cs + ce) / 2.0;
        let mut best_speaker: Option<u32> = None;
        let mut best_overlap: f64 = 0.0;

        for seg in &segments {
            let seg_start = seg.get("start").and_then(|s| s.as_f64()).unwrap_or(0.0);
            let seg_end = seg.get("end").and_then(|e| e.as_f64()).unwrap_or(0.0);
            let speaker = seg.get("speaker").and_then(|s| s.as_u64()).unwrap_or(0) as u32;

            // 计算重叠
            let overlap = (ce.min(seg_end) - cs.max(seg_start)).max(0.0);
            if overlap > best_overlap {
                best_overlap = overlap;
                best_speaker = Some(speaker);
            }

            // 如果 chunk 中点落在某段内，直接选它
            if chunk_mid >= seg_start && chunk_mid <= seg_end {
                best_speaker = Some(speaker);
                break;
            }
        }

        if let Some(speaker_id) = best_speaker {
            let speaker_label = format!("{}", (b'A' + speaker_id as u8) as char);
            let _ = sqlx::query("UPDATE chunks SET speaker = ? WHERE id = ?")
                .bind(&speaker_label)
                .bind(chunk_id)
                .execute(state.db.pool()).await;
            assigned += 1;
        }
    }

    eprintln!("[Diarization] 已分配 {} / {} 个 chunks", assigned, chunks.len());

    // 5. 推事件到前端，让前端重新渲染
    let _ = app_handle.emit("speakers_assigned", serde_json::json!({
        "meeting_id": meeting_id,
        "num_speakers": num_speakers,
        "assigned_chunks": assigned,
    }));

    Ok(serde_json::json!({
        "success": true,
        "num_speakers": num_speakers,
        "assigned_chunks": assigned,
        "segments": segments,
    }))
}

/// 拼接 seg_start..=seg_end 的段文件为一个 WAV（滚动精转 + 分片精转共用）
fn concat_seg_range_wav(session_dir: &str, seg_start: u32, seg_end: u32) -> Option<Vec<u8>> {
    let mut wav_parts: Vec<Vec<u8>> = Vec::new();
    let mut sample_rate: u32 = 16000;
    let mut num_channels: u16 = 1;
    let mut bits_per_sample: u16 = 16;

    for seg_num in seg_start..=seg_end {
        let seg_file = format!("{}/seg_{:03}.wav", session_dir, seg_num);
        match std::fs::read(&seg_file) {
            Ok(bytes) => {
                if bytes.len() < 1000 { continue; }
                // 提取音频数据（跳过WAV头）
                if wav_parts.is_empty() && bytes.len() >= 44 {
                    sample_rate = u32::from_le_bytes([bytes[24], bytes[25], bytes[26], bytes[27]]);
                    num_channels = u16::from_le_bytes([bytes[22], bytes[23]]);
                    bits_per_sample = u16::from_le_bytes([bytes[34], bytes[35]]);
                }
                // 找data chunk
                let mut data_start = 36;
                let mut i = 12;
                while i + 8 <= bytes.len() {
                    if &bytes[i..i+4] == b"data" {
                        data_start = i + 8;
                        break;
                    }
                    let chunk_size = u32::from_le_bytes([bytes[i+4], bytes[i+5], bytes[i+6], bytes[i+7]]) as usize;
                    i += 8 + chunk_size;
                }
                if data_start < bytes.len() {
                    wav_parts.push(bytes[data_start..].to_vec());
                }
            }
            Err(_) => continue,
        }
    }

    if wav_parts.is_empty() { return None; }

    // 构造完整WAV
    let total_data_len: usize = wav_parts.iter().map(|v| v.len()).sum();
    let total_len = 36 + total_data_len;
    let mut wav_bytes = Vec::with_capacity(44 + total_data_len);
    wav_bytes.extend_from_slice(b"RIFF");
    wav_bytes.extend_from_slice(&(total_len as u32).to_le_bytes());
    wav_bytes.extend_from_slice(b"WAVE");
    wav_bytes.extend_from_slice(b"fmt ");
    wav_bytes.extend_from_slice(&16u32.to_le_bytes());
    wav_bytes.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav_bytes.extend_from_slice(&num_channels.to_le_bytes());
    wav_bytes.extend_from_slice(&sample_rate.to_le_bytes());
    let byte_rate = sample_rate * num_channels as u32 * bits_per_sample as u32 / 8;
    wav_bytes.extend_from_slice(&byte_rate.to_le_bytes());
    let block_align = num_channels * bits_per_sample / 8;
    wav_bytes.extend_from_slice(&block_align.to_le_bytes());
    wav_bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav_bytes.extend_from_slice(b"data");
    wav_bytes.extend_from_slice(&(total_data_len as u32).to_le_bytes());
    for part in &wav_parts {
        wav_bytes.extend_from_slice(part);
    }
    Some(wav_bytes)
}

/// v3: 单引擎架构 — 录音停止后用 Qwen3-ASR 完整精转（/full_transcribe + context 热词），
/// 直接替换 chunks 表（交叉校验已停用，Qwen3 直出即为最终结果）
#[tauri::command]
pub async fn run_cross_validate_transcribe(
    app_handle: AppHandle,
    state: State<'_, AppState>,
    meeting_id: String,
    session_dir: String,
) -> Result<serde_json::Value, String> {
    if !crate::feature::is_feature_enabled(crate::feature::FeatureFlag::AsrPremium) {
        return Err("精转功能需要 Pro 版".to_string());
    }
    eprintln!("[FinalTranscribe] 开始 Qwen3-ASR 完整精转, meeting_id={}", meeting_id);

    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

    // 0. 收集段文件
    let mut segments: Vec<u32> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&session_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.starts_with("seg_") && name_str.ends_with(".wav") {
                if let Some(num_str) = name_str.strip_prefix("seg_").and_then(|s| s.strip_suffix(".wav")) {
                    if let Ok(num) = num_str.parse::<u32>() {
                        segments.push(num);
                    }
                }
            }
        }
    }
    segments.sort();
    if segments.is_empty() {
        return Ok(serde_json::json!({ "success": false, "message": "没有录音段文件", "final_segments": 0 }));
    }

    // 2. Qwen3-ASR 全量精转：窗口分片上传（2026-09-03 修复 50MB 限制 + 单次大文件超时）
    //    每片最多 300 段（10s/段 = 50 分钟），返回的 start/end 是相对片起点，需加片偏移
    const WINDOW_SEGS: u32 = 300;
    let hotword_context = db_get_hotword_context(&state.db, &meeting_id).await;

    // 3. 删除旧精转段（conf>=0.95），避免重复
    let _ = sqlx::query("DELETE FROM chunks WHERE meeting_id = ? AND confidence >= 0.95")
        .bind(&meeting_id)
        .execute(state.db.pool()).await;

    let mut total_segments = 0u32;
    let mut inserted = 0u32;
    let mut offset: usize = 0;
    let mut last_end: f64 = 0.0;
    let total_windows = ((segments.len() as f64) / (WINDOW_SEGS as f64)).ceil() as u32;
    let start_time = std::time::Instant::now();
    let mut window_idx: u32 = 0;

    // 精转开始事件
    let _ = app_handle.emit("refine_progress", serde_json::json!({
        "meeting_id": meeting_id,
        "phase": "start",
        "total_windows": total_windows,
    }));

    while offset < segments.len() {
        let win_start_seg = segments[offset];
        let win_end_seg = segments[(offset + WINDOW_SEGS as usize - 1).min(segments.len() - 1)];
        let wav_bytes = match concat_seg_range_wav(&session_dir, win_start_seg, win_end_seg) {
            Some(b) => b,
            None => {
                eprintln!("[CrossProcess] 窗口 {}..{} 无可用音频，跳过", win_start_seg, win_end_seg);
                offset += WINDOW_SEGS as usize;
                window_idx += 1;
                continue;
            }
        };
        let window_offset_secs = win_start_seg as f64 * SEG_DURATION_SEC;

        eprintln!("[CrossProcess] 分片精转 seg {}..{} ({}KB)", win_start_seg, win_end_seg, wav_bytes.len() / 1024);
        window_idx += 1;
        let elapsed = start_time.elapsed().as_secs();
        let _ = app_handle.emit("refine_progress", serde_json::json!({
            "meeting_id": meeting_id,
            "phase": "processing",
            "current": window_idx,
            "total": total_windows,
            "elapsed_secs": elapsed,
        }));
        let url = if hotword_context.is_empty() {
            format!("{}/final_transcribe", asr_url)
        } else {
            format!("{}/final_transcribe?context={}", asr_url, urlencode(&hotword_context))
        };
        let form = reqwest::multipart::Form::new()
            .part("file", reqwest::multipart::Part::bytes(wav_bytes.clone())
                .file_name("window.wav"));
        let resp = state.http_client.post(&url)
            .timeout(std::time::Duration::from_secs(1800))
            .multipart(form).send().await
            .map_err(|e| format!("Qwen3-ASR 请求失败: {}", e))?;
        let result: serde_json::Value = resp.json().await
            .map_err(|e| format!("Qwen3-ASR 响应解析失败: {}", e))?;
        let final_segments = result.get("segments").and_then(|s| s.as_array()).cloned().unwrap_or_default();

        // 写库（start/end 加窗口偏移）
        for seg in &final_segments {
            let text = seg.get("text").and_then(|t| t.as_str()).unwrap_or("");
            let start = seg.get("start").and_then(|s| s.as_f64()).unwrap_or(0.0) + window_offset_secs;
            let end = seg.get("end").and_then(|e| e.as_f64()).unwrap_or(0.0) + window_offset_secs;

            if text.is_empty() { continue; }
            let text = db_apply_hotwords(&text, &state.db).await;

            let cid = Uuid::new_v4().to_string();
            let _ = sqlx::query(
                "INSERT INTO chunks (id, meeting_id, start_time, end_time, transcript, speaker, confidence, word_count, processed_flag) VALUES (?, ?, ?, ?, ?, 'unknown', 0.95, ?, 1)"
            )
            .bind(&cid).bind(&meeting_id)
            .bind(start).bind(end)
            .bind(&text).bind(text.len() as i64)
            .execute(state.db.pool()).await;

            let jid = Uuid::new_v4().to_string();
            let _ = sqlx::query(
                "INSERT INTO jobs (id, meeting_id, type, status, payload) VALUES (?, ?, 'chunk_summary', 'pending', ?)"
            )
            .bind(&jid).bind(&meeting_id)
            .bind(serde_json::json!({"chunk_id": cid}).to_string())
            .execute(state.db.pool()).await;

            inserted += 1;
            if end > last_end { last_end = end; }
        }
        total_segments += final_segments.len() as u32;
        eprintln!("[CrossProcess] 窗口 {}..{} 完成: {} 段", win_start_seg, win_end_seg, final_segments.len());

        offset += WINDOW_SEGS as usize;
    }

    eprintln!("[CrossProcess] 分片精转全部完成: {} 片, 共插入 {} 段", (segments.len() as f64 / WINDOW_SEGS as f64).ceil() as u32, inserted);

    // 精转完成事件
    let _ = app_handle.emit("refine_progress", serde_json::json!({
        "meeting_id": meeting_id,
        "phase": "done",
        "total_segments": inserted,
        "elapsed_secs": start_time.elapsed().as_secs(),
    }));

    // 5. 推事件到前端，通知重新渲染
    let _ = app_handle.emit("cross_validate_done", serde_json::json!({
        "meeting_id": meeting_id,
        "final_segments": inserted,
    }));

    Ok(serde_json::json!({
        "success": true,
        "final_segments": inserted,
        "engine3_segments": total_segments,
    }))
}

/// 重命名说话人标签
#[tauri::command]
pub async fn rename_speaker(
    state: State<'_, AppState>,
    meeting_id: String,
    speaker_id: String,
    label: String,
) -> Result<serde_json::Value, String> {
    let id = Uuid::new_v4().to_string();
    // upsert: 先删后插
    let _ = sqlx::query("DELETE FROM speakers WHERE meeting_id = ? AND speaker_id = ?")
        .bind(&meeting_id).bind(&speaker_id)
        .execute(state.db.pool()).await;
    sqlx::query("INSERT INTO speakers (id, meeting_id, speaker_id, label) VALUES (?, ?, ?, ?)")
        .bind(&id).bind(&meeting_id).bind(&speaker_id).bind(&label)
        .execute(state.db.pool()).await
        .map_err(|e| e.to_string())?;

    // 同时更新 chunks 表中的 speaker 字段
    let _ = sqlx::query("UPDATE chunks SET speaker = ? WHERE meeting_id = ? AND speaker = ?")
        .bind(&label).bind(&meeting_id).bind(&speaker_id)
        .execute(state.db.pool()).await;

    Ok(serde_json::json!({"success": true}))
}

// ========== P5-4: 声纹库管理 ==========

/// 注册声纹：上传音频 + 说话人名字
#[tauri::command]
pub async fn enroll_speaker(
    state: State<'_, AppState>,
    name: String,
    audio_path: String,
) -> Result<serde_json::Value, String> {
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

    let bytes = tokio::fs::read(&audio_path).await
        .map_err(|e| format!("读取音频文件失败: {}", e))?;

    let form = reqwest::multipart::Form::new()
        .text("name", name.clone())
        .part("file", reqwest::multipart::Part::bytes(bytes).file_name("voice.wav"));

    let resp = state.http_client.post(format!("{}/enroll_speaker", asr_url))
        .multipart(form).send().await
        .map_err(|e| format!("声纹注册请求失败: {}", e))?;

    let result: serde_json::Value = resp.json().await
        .map_err(|e| format!("声纹注册响应解析失败: {}", e))?;

    let _ = tokio::fs::remove_file(&audio_path).await;
    Ok(result)
}

/// 列出已注册声纹
#[tauri::command]
pub async fn list_voiceprints(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

    let resp = state.http_client.get(format!("{}/voiceprints", asr_url))
        .send().await
        .map_err(|e| format!("请求失败: {}", e))?;

    let result: serde_json::Value = resp.json().await
        .map_err(|e| format!("解析失败: {}", e))?;

    Ok(result)
}

/// 删除声纹
#[tauri::command]
pub async fn delete_voiceprint(
    state: State<'_, AppState>,
    name: String,
) -> Result<serde_json::Value, String> {
    let asr_url = std::env::var("BIJIAN_SHERPA_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

    let resp = state.http_client.post(format!("{}/voiceprints/delete", asr_url))
        .json(&serde_json::json!({"name": name}))
        .send().await
        .map_err(|e| format!("请求失败: {}", e))?;

    let result: serde_json::Value = resp.json().await
        .map_err(|e| format!("解析失败: {}", e))?;

    Ok(result)
}

// ========== P5-5: 热词/词元库管理 ==========

/// 添加热词
#[tauri::command]
pub async fn add_hotword(
    state: State<'_, AppState>,
    wrong_text: String,
    correct_text: String,
    priority: Option<i32>,
    scene: Option<String>,
) -> Result<serde_json::Value, String> {
    let id = Uuid::new_v4().to_string();
    let priority = priority.unwrap_or(0);
    let scene = scene.unwrap_or_else(|| "general".to_string());
    sqlx::query("INSERT INTO hotwords (id, wrong_text, correct_text, priority, source, scene, frequency, confidence) VALUES (?, ?, ?, ?, 'manual', ?, 1, 1.0)")
        .bind(&id).bind(&wrong_text).bind(&correct_text).bind(priority).bind(&scene)
        .execute(state.db.pool()).await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"success": true, "id": id}))
}

/// 列出所有热词
#[tauri::command]
pub async fn list_hotwords(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let rows = sqlx::query("SELECT id, wrong_text, correct_text, priority, source, scene, frequency, confidence, created_at FROM hotwords ORDER BY priority DESC, created_at DESC")
        .fetch_all(state.db.pool()).await
        .map_err(|e| e.to_string())?;

    let hotwords: Vec<serde_json::Value> = rows.iter().map(|r| {
        serde_json::json!({
            "id": r.get::<String, _>("id"),
            "wrong_text": r.get::<String, _>("wrong_text"),
            "correct_text": r.get::<String, _>("correct_text"),
            "priority": r.get::<i32, _>("priority"),
            "scene": r.get::<String, _>("scene"),
            "frequency": r.get::<i32, _>("frequency"),
            "confidence": r.get::<f64, _>("confidence"),
            "source": r.get::<String, _>("source"),
            "created_at": r.get::<String, _>("created_at"),
        })
    }).collect();

    Ok(serde_json::json!({"hotwords": hotwords}))
}

/// 删除热词
#[tauri::command]
pub async fn delete_hotword(
    state: State<'_, AppState>,
    hotword_id: String,
) -> Result<serde_json::Value, String> {
    sqlx::query("DELETE FROM hotwords WHERE id = ?")
        .bind(&hotword_id)
        .execute(state.db.pool()).await
        .map_err(|e| e.to_string())?;
    Ok(serde_json::json!({"success": true}))
}

/// 批量导入热词
#[tauri::command]
pub async fn import_hotwords(
    state: State<'_, AppState>,
    hotwords: Vec<serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let mut count = 0;
    for hw in &hotwords {
        let wrong = hw.get("wrong_text").and_then(|t| t.as_str()).unwrap_or("");
        let correct = hw.get("correct_text").and_then(|t| t.as_str()).unwrap_or("");
        if !wrong.is_empty() && !correct.is_empty() {
            let id = Uuid::new_v4().to_string();
            let priority = hw.get("priority").and_then(|p| p.as_i64()).unwrap_or(0) as i32;
            let scene = hw.get("scene").and_then(|s| s.as_str()).unwrap_or("general").to_string();
            let _ = sqlx::query("INSERT INTO hotwords (id, wrong_text, correct_text, priority, source, scene) VALUES (?, ?, ?, ?, 'imported', ?)")
                .bind(&id).bind(wrong).bind(correct).bind(priority).bind(&scene)
                .execute(state.db.pool()).await;
            count += 1;
        }
    }
    Ok(serde_json::json!({"success": true, "imported": count}))
}

// ========== 截图 ==========

/// 截图命令：调用系统 screencapture 交互框选，保存到应用数据目录 screenshots/，返回图片绝对路径
/// - interactive: true（默认）用 -i 框选；false 用全屏
#[tauri::command]
pub async fn capture_screenshot(
    app: AppHandle,
    interactive: Option<bool>,
) -> Result<serde_json::Value, String> {
    let interactive = interactive.unwrap_or(true);

    // 截图保存目录：应用数据目录/screenshots
    let data_dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("获取数据目录失败: {e}"))?;
    let shot_dir = data_dir.join("screenshots");
    std::fs::create_dir_all(&shot_dir).map_err(|e| format!("创建截图目录失败: {e}"))?;

    // 文件名：screenshot_YYYYMMDD_HHMMSS.png
    let ts = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let file_path = shot_dir.join(format!("screenshot_{ts}.png"));
    let path_str = file_path.to_string_lossy().to_string();

    // 调用系统 screencapture：-i 交互框选，-x 不播放声音
    let mut cmd = std::process::Command::new("/usr/sbin/screencapture");
    cmd.arg("-x");
    if interactive {
        cmd.arg("-i");
    }
    cmd.arg(&path_str);

    let status = cmd
        .status()
        .map_err(|e| format!("调用 screencapture 失败: {e}"))?;

    if !status.success() {
        return Err("截图取消或失败（可能是未授予屏幕录制权限）".to_string());
    }

    if !file_path.exists() {
        return Err("截图未生成文件".to_string());
    }

    Ok(serde_json::json!({
        "path": path_str,
        "filename": file_path.file_name().unwrap_or_default().to_string_lossy().to_string(),
    }))
}

// ========== 许可证管理 ==========

#[tauri::command]
pub async fn activate_license(
    key: String,
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let result = crate::license::verify_license_key(&key);
    if result.success {
        let app_data_dir = app_handle
            .path()
            .app_data_dir()
            .unwrap_or_default();
        crate::license::save_license(&key, &app_data_dir)
            .map_err(|e| format!("激活成功但保存失败: {e}"))?;
    }
    Ok(serde_json::json!({
        "success": result.success,
        "message": result.message,
        "tier": result.tier,
        "expires_at": result.expires_at,
        "expires_human": result.expires_human,
    }))
}

#[tauri::command]
pub async fn get_license_status() -> Result<serde_json::Value, String> {
    let status = crate::feature::get_license_status();
    Ok(serde_json::json!(status))
}

#[tauri::command]
pub async fn deactivate_license(app_handle: AppHandle) -> Result<serde_json::Value, String> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .unwrap_or_default();
    crate::license::clear_license(&app_data_dir)
        .map_err(|e| format!("退出激活失败: {e}"))?;
    Ok(serde_json::json!({
        "success": true,
        "message": "已退出 Pro 模式",
    }))
}

// ========== 功能门控查询 ==========

#[tauri::command]
pub async fn check_feature(feature: String) -> Result<serde_json::Value, String> {
    let flag = match feature.as_str() {
        "ai_chat" => crate::feature::FeatureFlag::AiChat,
        "voice_transcribe" => crate::feature::FeatureFlag::VoiceTranscribe,
        "notes" => crate::feature::FeatureFlag::Notes,
        "asr_basic" => crate::feature::FeatureFlag::AsrBasic,
        "asr_premium" => crate::feature::FeatureFlag::AsrPremium,
        "speaker_diarization" => crate::feature::FeatureFlag::SpeakerDiarization,
        "meeting_summary" => crate::feature::FeatureFlag::MeetingSummary,
        "web_search" => crate::feature::FeatureFlag::WebSearch,
        "multi_model" => crate::feature::FeatureFlag::MultiModel,
        "batch_import" => crate::feature::FeatureFlag::BatchImport,
        _ => return Err(format!("未知的功能标识: {feature}")),
    };
    Ok(serde_json::json!({
        "feature": feature,
        "enabled": crate::feature::is_feature_enabled(flag),
        "name": flag.name(),
    }))
}

// v2.3.2: 应用设置（跨重装可恢复，从 localStorage 迁 SQLite）
#[tauri::command]
pub async fn get_setting(state: State<'_, AppState>, key: String) -> Result<Option<String>, String> {
    let row = sqlx::query("SELECT value FROM settings WHERE key = ?").bind(&key)
        .fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(row.map(|r| r.get::<String, _>("value")))
}

#[tauri::command]
pub async fn set_setting(state: State<'_, AppState>, key: String, value: String) -> Result<(), String> {
    sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, datetime('now')) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')")
        .bind(&key).bind(&value)
        .execute(state.db.pool()).await.map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_settings(state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let rows = sqlx::query("SELECT key, value FROM settings").fetch_all(state.db.pool()).await.map_err(|e| e.to_string())?;
    let mut m = serde_json::Map::new();
    for r in rows {
        m.insert(r.get::<String, _>("key"), serde_json::Value::String(r.get::<String, _>("value")));
    }
    Ok(serde_json::Value::Object(m))
}

// ========== 数据备份与恢复 ==========

/// 备份数据库到指定路径
#[tauri::command]
pub async fn backup_database(
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .unwrap_or_default();

    let backup_dir = app_data_dir.join("backups");
    tokio::fs::create_dir_all(&backup_dir).await
        .map_err(|e| format!("创建备份目录失败: {}", e))?;

    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S");
    let backup_filename = format!("pannote_backup_{}.db", timestamp);
    let backup_path = backup_dir.join(&backup_filename);

    // 使用 VACUUM INTO 创建紧凑备份
    let backup_path_str = backup_path.to_string_lossy().to_string();
    let state = app_handle.state::<crate::state::AppState>();
    state.db.backup_to(&backup_path_str).await?;

    // 清理 30 天以上的旧备份
    if let Ok(mut entries) = tokio::fs::read_dir(&backup_dir).await {
        let cutoff = chrono::Utc::now().timestamp() - 30 * 24 * 3600;
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                if let Ok(modified) = meta.modified() {
                    let ts = modified.duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(0);
                    if ts < cutoff {
                        let _ = tokio::fs::remove_file(entry.path()).await;
                        eprintln!("[Backup] 清理过期备份: {:?}", entry.file_name());
                    }
                }
            }
        }
    }

    // 列出现有备份
    let mut backups: Vec<serde_json::Value> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&backup_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            if let Ok(meta) = entry.metadata().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("pannote_backup_") {
                    let size_kb = meta.len() as f64 / 1024.0;
                    let modified = meta.modified()
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH)
                            .map(|d| format!("{}", chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                                .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                .unwrap_or_default()))
                            .unwrap_or_default())
                        .unwrap_or_default();
                    backups.push(serde_json::json!({
                        "filename": name,
                        "size_kb": format!("{:.1}", size_kb),
                        "created_at": modified,
                    }));
                }
            }
        }
    }
    backups.sort_by(|a, b| b["filename"].as_str().cmp(&a["filename"].as_str()));

    // 保留最近 10 个备份
    for old in backups.iter().skip(10) {
        let path = backup_dir.join(old["filename"].as_str().unwrap_or(""));
        let _ = tokio::fs::remove_file(&path).await;
    }

    // 返回最新 backup 列表（最多 10 个）
    let backups_trimmed: Vec<&serde_json::Value> = backups.iter().take(10).collect();

    Ok(serde_json::json!({
        "success": true,
        "message": "备份成功",
        "backup_path": backup_path_str,
        "backup_filename": backup_filename,
        "backups": backups_trimmed,
    }))
}

// ========== v2.5.1 每日自动备份（db + 近 7 天音频，失败强提醒）==========
// 背景：9-24 三场会议音频永久丢失（麦克风全零且无备份）。音频是会议唯一原始记录，
// 备份不再是可选项。策略：应用运行期间每小时检查一次"今日是否已备份"，未备则执行——
// 比固定时刻（07:00）更稳，Mac 不必定点开机。幂等标记存 settings 表。

/// 三套历史 audio_cache 目录（目录统一前的兼容扫描：PanNote / 笔尖 / com.bijian.app）
fn all_audio_cache_dirs() -> Vec<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    ["PanNote", "笔尖", "com.bijian.app"]
        .iter()
        .map(|name| std::path::PathBuf::from(format!("{}/Library/Application Support/{}/audio_cache", home, name)))
        .collect()
}

/// 每日自动备份主入口：db（VACUUM INTO）+ 近 7 天会议音频目录整体复制
pub async fn run_auto_backup_if_needed(app_handle: &tauri::AppHandle) {
    let state = app_handle.state::<crate::state::AppState>();
    let db = state.db.clone();
    drop(state);

    let today = chrono::Local::now().format("%Y-%m-%d").to_string();

    // 幂等：今天已备份则跳过
    let already = sqlx::query("SELECT value FROM settings WHERE key = 'last_auto_backup_date'")
        .fetch_optional(db.pool()).await.ok().flatten();
    if let Some(r) = already {
        if r.get::<String, _>("value") == today {
            return;
        }
    }

    let app_data_dir = app_handle.path().app_data_dir().unwrap_or_default();
    let backup_root = app_data_dir.join("backups").join(format!("auto_{}", today.replace('-', "")));
    let audio_backup_dir = backup_root.join("audio");

    let start = std::time::Instant::now();
    let mut copied_files: u64 = 0;
    let mut copied_bytes: u64 = 0;
    let mut errors: Vec<String> = Vec::new();

    // 1) 备份 db
    let db_backup_path = backup_root.join("pannote.db");
    if let Err(e) = db.backup_to(&db_backup_path.to_string_lossy()).await {
        let msg = format!("自动备份失败（db）: {}", e);
        crate::logger::log("AutoBackup", &msg);
        let _ = app_handle.emit("auto_backup_failed", serde_json::json!({ "reason": msg, "stage": "db" }));
        return;
    }

    // 2) 备份近 7 天会议音频（按会议目录 mtime 判断，整目录复制）
    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
        - 7 * 24 * 3600;
    for cache_dir in all_audio_cache_dirs() {
        let mut entries = match tokio::fs::read_dir(&cache_dir).await {
            Ok(e) => e,
            Err(_) => continue, // 目录不存在（正常，三选一）
        };
        while let Ok(Some(entry)) = entries.next_entry().await {
            let p = entry.path();
            // 只备会议目录（rec_ 前缀），跳过 tmp/碎片
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with("rec_") { continue; }
            let mtime = entry.metadata().await
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            if mtime < cutoff { continue; } // 近 7 天内无修改的旧会议不备（省空间）
            let dest = audio_backup_dir.join(&name);
            if let Err(e) = copy_dir_recursive(&p, &dest).await {
                errors.push(format!("{}: {}", name, e));
                continue;
            }
        }
    }

    // 统计复制结果
    if audio_backup_dir.exists() {
        if let Ok(mut entries) = tokio::fs::read_dir(&audio_backup_dir).await {
            while let Ok(Some(e)) = entries.next_entry().await {
                count_dir(&e.path(), &mut copied_files, &mut copied_bytes).await;
            }
        }
    }

    // 3) 清理：只保留最近 7 个 auto_* 备份目录
    cleanup_auto_backups(&app_data_dir.join("backups")).await;

    // 4) 幂等标记 + 事件
    let _ = sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES ('last_auto_backup_date', ?, datetime('now')) ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = datetime('now')")
        .bind(&today)
        .execute(db.pool()).await;

    let elapsed = start.elapsed().as_secs();
    let summary = serde_json::json!({
        "date": today,
        "db_backup": db_backup_path.to_string_lossy(),
        "audio_meeting_dirs": { "files": copied_files, "bytes": copied_bytes },
        "warnings": errors,
        "elapsed_secs": elapsed,
    });
    crate::logger::log("AutoBackup", &format!("自动备份完成：db + {} 个音频文件（{}KB），耗时 {}s", copied_files, copied_bytes / 1024, elapsed));
    // 成功也广播（前端弱提示）；音频部分错误不视为整体失败，作为 warnings 透出
    let _ = app_handle.emit("auto_backup_completed", &summary);
}

async fn copy_dir_recursive(src: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    tokio::fs::create_dir_all(dest).await.map_err(|e| format!("创建目录失败: {}", e))?;
    let mut entries = tokio::fs::read_dir(src).await.map_err(|e| format!("读目录失败: {}", e))?;
    while let Some(entry) = entries.next_entry().await.map_err(|e| e.to_string())? {
        let ty = entry.file_type().await.map_err(|e| e.to_string())?;
        let to = dest.join(entry.file_name());
        if ty.is_dir() {
            Box::pin(copy_dir_recursive(&entry.path(), &to)).await?;
        } else {
            tokio::fs::copy(entry.path(), &to).await.map_err(|e| format!("复制失败: {}", e))?;
        }
    }
    Ok(())
}

async fn count_dir(p: &std::path::Path, files: &mut u64, bytes: &mut u64) {
    if let Ok(meta) = tokio::fs::metadata(p).await {
        if meta.is_file() { *files += 1; *bytes += meta.len(); return; }
    }
    if let Ok(mut entries) = tokio::fs::read_dir(p).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            Box::pin(count_dir(&e.path(), files, bytes)).await;
        }
    }
}

async fn cleanup_auto_backups(backups_dir: &std::path::Path) {
    let mut autos: Vec<(String, std::path::PathBuf)> = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(backups_dir).await {
        while let Ok(Some(e)) = entries.next_entry().await {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with("auto_") {
                autos.push((name, e.path()));
            }
        }
    }
    autos.sort_by(|a, b| b.0.cmp(&a.0)); // 新的在前
    for (_, path) in autos.iter().skip(7) {
        let _ = tokio::fs::remove_dir_all(path).await;
    }
}

/// v2.5.2 转写状态查询（状态卡片数据源）：段级状态分布 + 引擎分布 + fallback 计数 + 主表统计
/// 前端详情页/诊断入口按需调用（不进列表查询，避免每会议子查询开销）
#[tauri::command]
pub async fn get_transcription_stats(state: State<'_, AppState>, meeting_id: String) -> Result<serde_json::Value, String> {
    let rows: Vec<(String, i64)> = sqlx::query(
        "SELECT state, COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? GROUP BY state"
    ).bind(&meeting_id).fetch_all(state.db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<String,_>("state"), r.get::<i64,_>("c"))).collect())
    .map_err(|e| e.to_string())?;
    let engines: Vec<(String, i64)> = sqlx::query(
        "SELECT engine, COUNT(*) as c FROM transcription_segments WHERE meeting_id = ? AND state='done' AND engine != '' GROUP BY engine"
    ).bind(&meeting_id).fetch_all(state.db.pool()).await
    .map(|rs| rs.iter().map(|r| (r.get::<String,_>("engine"), r.get::<i64,_>("c"))).collect())
    .map_err(|e| e.to_string())?;
    let main: Option<(String, i64, i64, i64)> = sqlx::query(
        "SELECT status, expected_segments, completed_segments, failed_segments FROM meetings WHERE id = ?"
    ).bind(&meeting_id).fetch_optional(state.db.pool()).await
    .map(|rs| rs.map(|r| (r.get::<String,_>("status"), r.get::<i64,_>("expected_segments"), r.get::<i64,_>("completed_segments"), r.get::<i64,_>("failed_segments"))))
    .map_err(|e| e.to_string())?;

    let mut by_state = serde_json::Map::new();
    let mut total = 0i64;
    for (st, c) in &rows { by_state.insert(st.clone(), serde_json::json!(c)); total += c; }
    let mut engine_map = serde_json::Map::new();
    let mut fallback_count = 0i64;
    for (e, c) in &engines {
        if e.starts_with("fallback_") { fallback_count += c; }
        engine_map.insert(e.clone(), serde_json::json!(c));
    }
    let (status, expected, completed, failed) = match main {
        Some(r) => r,
        None => return Err("会议不存在".to_string()),
    };
    Ok(serde_json::json!({
        "meeting_id": meeting_id,
        "status": status,
        "expected_segments": expected,
        "completed_segments": completed,
        "failed_segments": failed,
        "total": total,
        "by_state": by_state,
        "engines": engine_map,
        "fallback_count": fallback_count,
    }))
}

/// v2.5.2 失败段一键重试：failed → pending（清错误计数）后交给补跑差集扫描重新转写。
/// 重置后的 pending 段无终态行记录，complete_transcribe 会将其纳入差集——复用既有补跑闭环。
#[tauri::command]
pub async fn retry_failed_segments<R: tauri::Runtime>(state: State<'_, AppState>, app_handle: AppHandle<R>, meeting_id: String) -> Result<serde_json::Value, String> {
    let reset = sqlx::query(
        "UPDATE transcription_segments SET state='pending', last_error='', attempts=0, started_at='', finished_at='' WHERE meeting_id=? AND state='failed'"
    ).bind(&meeting_id).execute(state.db.pool()).await
    .map(|r| r.rows_affected()).map_err(|e| e.to_string())?;
    if reset == 0 {
        return Ok(serde_json::json!({ "reset_count": 0, "message": "没有失败段" }));
    }
    let sd: Option<String> = sqlx::query("SELECT session_dir FROM meetings WHERE id=?").bind(&meeting_id)
        .fetch_optional(state.db.pool()).await.map_err(|e| e.to_string())?
        .map(|r| r.get("session_dir"));
    let sd = match sd {
        Some(s) if !s.is_empty() => s,
        _ => {
            crate::logger::log("Retry", &format!("会议 {} 已重置 {} 段，但无录音目录，等待巡检接管", &meeting_id, reset));
            return Ok(serde_json::json!({ "reset_count": reset, "message": "已重置失败段，等待巡检自动补转" }));
        }
    };
    let short_id: String = meeting_id.chars().take(8).collect();
    crate::logger::log("Retry", &format!("会议 {} 重试 {} 个失败段，补转启动", short_id, reset));
    let db = state.db.clone();
    let http = state.http_client.clone();
    let mid = meeting_id.clone();
    let sd2 = sd.clone();
    let ah = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        complete_transcribe(&sd2, &mid, &db, &http, &ah).await;
    });
    Ok(serde_json::json!({ "reset_count": reset, "message": format!("已重置 {} 个失败段，补转已启动", reset) }))
}

/// 从备份恢复数据库
#[tauri::command]
pub async fn restore_database(
    app_handle: AppHandle,
    backup_filename: String,
) -> Result<serde_json::Value, String> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .unwrap_or_default();

    let backup_path = app_data_dir.join("backups").join(&backup_filename);

    if !backup_path.exists() {
        return Err(format!("备份文件不存在: {}", backup_filename));
    }

    let db_path = app_data_dir.join("history.db");

    // 关闭当前数据库连接（通过 VACUUM 不行，需要直接覆盖文件）
    // 先备份当前数据库（恢复前的状态，方便撤销恢复）
    let pre_restore_path = app_data_dir.join("backups").join(format!("pre_restore_{}.db",
        chrono::Local::now().format("%Y%m%d_%H%M%S")));

    // 用文件复制方式备份当前数据库
    if db_path.exists() {
        tokio::fs::copy(&db_path, &pre_restore_path).await
            .map_err(|e| format!("备份当前数据库失败: {}", e))?;
    }

    // 覆盖数据库文件
    tokio::fs::copy(&backup_path, &db_path).await
        .map_err(|e| format!("恢复数据库失败: {}", e))?;

    Ok(serde_json::json!({
        "success": true,
        "message": "数据库已恢复，请重启应用以使恢复生效",
        "pre_restore_backup": pre_restore_path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
    }))
}

/// 列出所有备份
#[tauri::command]
pub async fn list_backups(
    app_handle: AppHandle,
) -> Result<serde_json::Value, String> {
    let app_data_dir = app_handle
        .path()
        .app_data_dir()
        .unwrap_or_default();

    let backup_dir = app_data_dir.join("backups");

    let mut backups: Vec<serde_json::Value> = Vec::new();
    if backup_dir.exists() {
        if let Ok(mut entries) = tokio::fs::read_dir(&backup_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                if let Ok(meta) = entry.metadata().await {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if name.starts_with("pannote_backup_") || name.starts_with("pre_restore_") {
                        let size_kb = meta.len() as f64 / 1024.0;
                        let modified = meta.modified()
                            .map(|t| t.duration_since(std::time::UNIX_EPOCH)
                                .map(|d| chrono::DateTime::<chrono::Utc>::from_timestamp(d.as_secs() as i64, 0)
                                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                                    .unwrap_or_default())
                                .unwrap_or_default())
                            .unwrap_or_default();
                        backups.push(serde_json::json!({
                            "filename": name,
                            "size_kb": format!("{:.1}", size_kb),
                            "created_at": modified,
                        }));
                    }
                }
            }
        }
    }
    backups.sort_by(|a, b| b["filename"].as_str().cmp(&a["filename"].as_str()));

    Ok(serde_json::json!({
        "backups": backups,
        "backup_dir": backup_dir.to_string_lossy().to_string(),
    }))
}

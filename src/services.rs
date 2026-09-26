
// 笔尖APP - 服务模块

/// UTF-8 安全截断：在不超过 max_bytes 的前提下回退到最近的字符边界，
/// 避免截断到多字节字符中间导致 panic（原 `s[..N]` 字节截断的致命缺陷）
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

pub mod search {
    /// 网页搜索：四级降级链
    /// 1. 搜狗（www.sogou.com）—免费免Key，国内直连，中文分词好
    /// 2. 必应中国（cn.bing.com）—免费免Key，国内直连，备选
    /// 3. Tavily API（仅读环境变量 TAVILY_API_KEY，可选）
    /// 4. 返回带 error 字段的结果，让上层告知用户
    pub async fn web_search(query: &str) -> Result<Vec<serde_json::Value>, String> {
        // ---- 第一级：搜狗（直连，中文分词好）----
        match search_sogou(query).await {
            Ok(results) if !results.is_empty() => return Ok(results),
            Ok(_) => { /* 空结果，降级 */ }
            Err(e) => eprintln!("[search] 搜狗搜索失败，降级: {}", e),
        }

        // ---- 第二级：必应中国（直连，备选）----
        match search_bing(query).await {
            Ok(results) if !results.is_empty() => return Ok(results),
            Ok(_) => { /* 空结果，降级 */ }
            Err(e) => eprintln!("[search] 必应搜索失败，降级: {}", e),
        }

        // ---- 第三级：Tavily（需 API Key）----
        let api_key = match std::env::var("TAVILY_API_KEY") {
            Ok(k) if !k.is_empty() => k,
            _ => String::new(),
        };
        if !api_key.is_empty() {
            match search_tavily(query, &api_key).await {
                Ok(results) if !results.is_empty() => return Ok(results),
                Ok(_) => { /* 空结果，降级 */ }
                Err(e) => eprintln!("[search] Tavily 失败，降级: {}", e),
            }
        }

        // ---- 第四级：明确返回空 + 错误信息 ----
        Ok(vec![serde_json::json!({
            "source": "error",
            "title": "搜索不可用",
            "snippet": "所有搜索源均不可用。请检查网络连接。",
            "url": null,
        })])
    }

    /// 搜狗搜索：直连 www.sogou.com（不走代理），免Key，解析 HTML 结果页
    /// 搜狗中文分词优于必应，"最好用的编程大模型"不会被拆成"最好"
    async fn search_sogou(query: &str) -> Result<Vec<serde_json::Value>, String> {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()
            .build()
            .map_err(|e| format!("HTTP 客户端构建失败: {}", e))?;

        let url = format!("https://www.sogou.com/web?query={}", urlencoding::encode(query));
        eprintln!("[search] 搜狗请求: {}", url);
        let resp = client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send().await
            .map_err(|e| format!("搜狗请求失败: {}", e))?;

        let status = resp.status();
        eprintln!("[search] 搜狗响应状态: {}", status);
        if !status.is_success() {
            return Err(format!("搜狗 HTTP {}", status));
        }

        let html = resp.text().await.map_err(|e| format!("读取搜狗响应失败: {}", e))?;
        eprintln!("[search] 搜狗HTML长度: {}", html.len());
        let results = parse_sogou_html(&html);
        eprintln!("[search] 搜狗解析结果数: {}", results.len());
        Ok(results)
    }

    /// 解析搜狗搜索结果 HTML：
    /// 标题在 <h3 class="vr-title"><a href="URL">标题</a></h3>
    /// 摘要在标题后面的 vrwrap 块中的文本段落
    fn parse_sogou_html(html: &str) -> Vec<serde_json::Value> {
        let mut results = Vec::new();

        let title_re = regex::Regex::new(
            r#"<h3[^>]*class="[^"]*vr-title[^"]*"[^>]*>\s*<a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#
        ).unwrap();

        for cap in title_re.captures_iter(html) {
            let url = cap.get(1).map(|m| m.as_str().to_string()).unwrap_or_default();
            let title_raw = cap.get(2).map(|m| m.as_str().to_string()).unwrap_or_default();
            let title = decode_html_entities(&strip_html_tags(&title_raw));
            if title.is_empty() { continue; }

            // 从标题位置往后找摘要文本
            let match_end = cap.get(0).map(|m| m.end()).unwrap_or(0);
            let after = if html.len() > match_end { &html[match_end..] } else { "" };
            // 截取后续 3000 字符搜索摘要
            let search_zone = if after.len() > 3000 { &after[..3000] } else { after };
            let snippet = extract_sogou_snippet(search_zone);

            results.push(serde_json::json!({
                "source": "sogou",
                "title": title,
                "snippet": snippet,
                "url": url,
            }));
            if results.len() >= 5 { break; }
        }
        results
    }

    /// 从搜狗标题后的 HTML 片段中提取摘要文本
    fn extract_sogou_snippet(html_fragment: &str) -> String {
        let p_re = regex::Regex::new(r#"<(?:p|div)[^>]*>(.*?)</(?:p|div)>"#).unwrap();
        for cap in p_re.captures_iter(html_fragment) {
            let raw = cap.get(1).map(|m| m.as_str()).unwrap_or("");
            let clean = decode_html_entities(&strip_html_tags(raw));
            // 过滤掉导航/广告/相关推荐等无关文本
            if clean.len() > 30
                && !clean.contains("相关推荐")
                && !clean.contains("搜狗已为您")
                && !clean.contains("人气指数")
            {
                return clean;
            }
        }
        String::new()
    }

    /// 解码 HTML 实体：&lt; &gt; &amp; &ensp; &nbsp; &#xxx; 等
    fn decode_html_entities(s: &str) -> String {
        let mut result = s.to_string();
        result = result.replace("&lt;", "<");
        result = result.replace("&gt;", ">");
        result = result.replace("&amp;", "&");
        result = result.replace("&ensp;", " ");
        result = result.replace("&nbsp;", " ");
        result = result.replace("&quot;", "\"");
        result = result.replace("&#39;", "'");
        // 解码数字实体 &#123; 形式
        let num_re = regex::Regex::new(r"&#(\d+);").unwrap();
        result = num_re.replace_all(&result, |c: &regex::Captures| {
            let num: u32 = c.get(1).unwrap().as_str().parse().unwrap_or(0);
            if let Some(ch) = char::from_u32(num) { ch.to_string() } else { String::new() }
        }).to_string();
        result.trim().to_string()
    }

    /// 必应中国搜索：直连 cn.bing.com（不走代理），免Key，解析 HTML 结果页
    async fn search_bing(query: &str) -> Result<Vec<serde_json::Value>, String> {
        // 必应国内站必须直连——走代理会被路由到国际 Bing，搜索内容和区域都错
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .no_proxy()  // 强制不走系统代理
            .build()
            .map_err(|e| format!("HTTP 客户端构建失败: {}", e))?;

        let url = format!("https://cn.bing.com/search?q={}", urlencoding::encode(query));
        eprintln!("[search] 必应请求: {}", url);
        let resp = client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
            .header("Accept-Language", "zh-CN,zh;q=0.9,en;q=0.8")
            .send().await
            .map_err(|e| format!("必应请求失败: {}", e))?;

        let status = resp.status();
        eprintln!("[search] 必应响应状态: {}", status);
        if !status.is_success() {
            return Err(format!("必应 HTTP {}", status));
        }

        let html = resp.text().await.map_err(|e| format!("读取必应响应失败: {}", e))?;
        eprintln!("[search] 必应HTML长度: {}", html.len());
        let results = parse_bing_html(&html);
        eprintln!("[search] 必应解析结果数: {}", results.len());
        Ok(results)
    }

    /// 解析必应搜索结果 HTML：
    /// 标题在 <h2><a href="URL">标题</a></h2>
    /// 摘要在 <div class="b_caption">...<p>摘要</p>...</div>
    fn parse_bing_html(html: &str) -> Vec<serde_json::Value> {
        let mut results = Vec::new();

        // 提取标题+URL：h2 > a
        let title_re = regex::Regex::new(
            r#"<h2[^>]*><a[^>]*href="([^"]+)"[^>]*>(.*?)</a>"#
        ).unwrap();
        let titles: Vec<(String, String)> = title_re
            .captures_iter(html)
            .filter_map(|c| {
                let url = c.get(1)?.as_str().to_string();
                let title_raw = c.get(2)?.as_str().to_string();
                let title = strip_html_tags(&title_raw);
                if title.is_empty() { None } else { Some((url, title)) }
            })
            .collect();

        // 提取摘要：b_caption 下的 p 标签
        let snippet_re = regex::Regex::new(
            r#"<div class="b_caption"[^>]*>.*?<p[^>]*>(.*?)</p>"#
        ).unwrap();
        let snippets: Vec<String> = snippet_re
            .captures_iter(html)
            .map(|c| strip_html_tags(c.get(1).unwrap().as_str()))
            .collect();

        for (i, (url, title)) in titles.iter().enumerate() {
            let snippet = snippets.get(i).cloned().unwrap_or_default();
            results.push(serde_json::json!({
                "source": "bing",
                "title": title,
                "snippet": snippet,
                "url": url,
            }));
            if results.len() >= 5 { break; }
        }
        results
    }

    /// 构建 HTTP 客户端：读系统代理（HTTPS_PROXY/HTTP_PROXY），
    /// 有则走代理，无则直连。不设置全局 timeout（per-request 控制）。
    fn build_http_client() -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(15));

        // reqwest 默认不读系统代理，需要显式开启
        // 如果环境变量有代理设置，让 reqwest 使用它
        let has_proxy = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("HTTP_PROXY"))
            .or_else(|_| std::env::var("https_proxy")).or_else(|_| std::env::var("http_proxy"))
            .map(|_| true).unwrap_or(false);
        if has_proxy {
            builder = builder.connection_verbose(false);
            // reqwest 的 .proxy(reqwest::Proxy::all(...)) 在编译期需要 "default" feature
            // 这里用更简单的方式：让 reqwest 读环境变量（通过 proxy 系统配置）
            // reqwest 0.12 默认不自动读 env，所以我们手动设置
            let proxy_url = std::env::var("HTTPS_PROXY").or_else(|_| std::env::var("https_proxy"))
                .or_else(|_| std::env::var("HTTP_PROXY")).or_else(|_| std::env::var("http_proxy"))
                .unwrap_or_default();
            if !proxy_url.is_empty() {
                match reqwest::Proxy::all(&proxy_url) {
                    Ok(p) => { builder = builder.proxy(p); }
                    Err(e) => eprintln!("[search] 代理配置失败，直连: {}", e),
                }
            }
        }

        builder.build().map_err(|e| format!("HTTP 客户端构建失败: {}", e))
    }

    async fn search_tavily(query: &str, api_key: &str) -> Result<Vec<serde_json::Value>, String> {
        let client = build_http_client()?;
        let response = client
            .post("https://api.tavily.com/search")
            .json(&serde_json::json!({
                "api_key": api_key,
                "query": query,
                "max_results": 5,
                "include_answer": true
            }))
            .timeout(std::time::Duration::from_secs(15))
            .send().await
            .map_err(|e| e.to_string())?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Tavily HTTP {}: {}", status, body));
        }

        let result: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;

        let mut results = Vec::new();

        if let Some(answer) = result.get("answer").and_then(|a| a.as_str()) {
            if !answer.is_empty() {
                results.push(serde_json::json!({
                    "source": "tavily",
                    "title": "AI 摘要",
                    "snippet": answer,
                    "url": null,
                }));
            }
        }

        if let Some(search_results) = result.get("results").and_then(|r| r.as_array()) {
            for r in search_results.iter().take(5) {
                results.push(serde_json::json!({
                    "source": "tavily",
                    "title": r.get("title").and_then(|t| t.as_str()).unwrap_or(""),
                    "snippet": r.get("content").and_then(|c| c.as_str()).unwrap_or(""),
                    "url": r.get("url").and_then(|u| u.as_str()),
                }));
            }
        }

        Ok(results)
    }

    fn strip_html_tags(s: &str) -> String {
        let re = regex::Regex::new(r#"<[^>]+>"#).unwrap();
        re.replace_all(s, "").trim().to_string()
    }
}

pub mod evolution {
    pub struct EvolutionSystem;
    impl EvolutionSystem {
        pub fn new() -> Self { Self }
    }
}

pub mod tools {
    // Function calling tools
}

pub mod worker {
    /// Worker 聚合任务最大重试次数（防止 ASR/LLM 故障时 pending→processing 死循环）
    const MAX_JOB_ATTEMPTS: i64 = 20;

    use tauri::{AppHandle, Emitter, Manager};
    use sqlx::Row;
    use uuid::Uuid;

    pub fn start_worker(app_handle: AppHandle) {
        tauri::async_runtime::spawn(async move {
            let state = app_handle.state::<crate::state::AppState>();
            let db = state.db.clone();
            let http_client = state.http_client.clone();
            let ollama_url = std::env::var("OLLAMA_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:11434".to_string());
            let model = "qwen3-4b-32k".to_string();
            // v2.4.5: 标记活跃请求，防止 keepalive 空闲卸载模型
            state.ollama_manager.touch();

            loop {
                // v2.5.2 连续消费模式：队列有单立即取下一单（不 sleep），队列空才睡 2s。
                // 根治"2h 会议 720 段 × 每 2s 才取 1 单"的纯轮询滞后（原 ~24 分钟等待归零）。
                // 不做并发多单：CPU 上 4B 模型并发反而互相拖慢，chunk_summary 保序更稳。
                // 获取 pending job
                let job = sqlx::query(
                    "SELECT id, meeting_id, type, payload FROM jobs WHERE status = 'pending' ORDER BY created_at ASC LIMIT 1"
                )
                .fetch_optional(db.pool()).await.ok().flatten();

                let job = match job {
                    Some(j) => j,
                    None => {
                        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                        continue;
                    }
                };

                let job_id: String = j_get(&job, "id");
                let meeting_id: String = j_get(&job, "meeting_id");
                let job_type: String = j_get(&job, "type");

                // 标记为 processing
                let _ = sqlx::query("UPDATE jobs SET status = 'processing', updated_at = datetime('now') WHERE id = ?")
                    .bind(&job_id)
                    .execute(db.pool()).await;

                let event_name = format!("meeting_progress_{}", meeting_id);

                if job_type == "chunk_summary" {
                    let payload: serde_json::Value = serde_json::from_str(j_get(&job, "payload").as_str()).unwrap_or_default();
                    let chunk_id = payload.get("chunk_id").and_then(|c| c.as_str()).unwrap_or("");

                    // 获取 chunk 文本
                    let chunk = sqlx::query("SELECT transcript, speaker FROM chunks WHERE id = ?")
                        .bind(chunk_id)
                        .fetch_optional(db.pool()).await.ok().flatten();

                    if let Some(chunk_row) = chunk {
                        let text: String = chunk_row.get("transcript");
                        let _speaker: String = chunk_row.get("speaker");

                        // P5-3: 标点恢复改用 ct-transformer（毫秒级），摘要仍用 LLM
                        let asr_url = std::env::var("BIJIAN_SHERPA_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

                        // 调 /punctuate 端点（ct-transformer 标点恢复）
                        let punctuated_text = if !text.is_empty() {
                            match http_client.post(format!("{}/punctuate", asr_url))
                                .json(&serde_json::json!({"text": text}))
                                .timeout(std::time::Duration::from_secs(10))
                                .send().await
                            {
                                Ok(resp) => {
                                    let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                    result.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string()
                                }
                                Err(e) => {
                                    eprintln!("[Worker] 标点恢复失败，用原文: {}", e);
                                    text.clone()
                                }
                            }
                        } else {
                            String::new()
                        };

                        // 只做标点恢复（ct-transformer，毫秒级，不耗 4B）；摘要仅由用户点「生成纪要」时触发
                        // P3: 写入标点恢复后的文本
                        if !punctuated_text.is_empty() {
                            let _ = sqlx::query("UPDATE chunks SET punctuated_text = ?, processed_flag = 1 WHERE id = ?")
                                .bind(&punctuated_text).bind(chunk_id)
                                .execute(db.pool()).await;
                            // 推事件到前端，通知标点恢复完成
                            let _ = app_handle.emit(&event_name, serde_json::json!({
                                "type": "punctuation_done",
                                "chunk_id": chunk_id,
                                "punctuated_text": punctuated_text,
                            }));
                        } else {
                            let _ = sqlx::query("UPDATE chunks SET processed_flag = 1 WHERE id = ?")
                                .bind(chunk_id)
                                .execute(db.pool()).await;
                        }
                    }

                    let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                        .bind(&job_id)
                        .execute(db.pool()).await;

                    let _ = app_handle.emit(&event_name, serde_json::json!({"type": "chunk_summary_done", "chunk_id": chunk_id}));

                } else if job_type == "aggregate" {
                    // 解析 payload 中的 template_type
                    let payload_val: serde_json::Value = serde_json::from_str(j_get(&job, "payload").as_str()).unwrap_or_default();
                    let template_type = payload_val.get("template_type").and_then(|t| t.as_str()).unwrap_or("meeting");
                    // v2.5.2 部分转写标注：trigger_aggregate 段级门槛 force 放行时携带，注入纪要 JSON 的 partial_note 字段
                    let partial_note = payload_val.get("partial_note").and_then(|p| p.as_str()).unwrap_or("").to_string();

                    // 检查所有 chunk 是否已处理
                    let pending_chunks = sqlx::query("SELECT COUNT(*) as cnt FROM chunks WHERE meeting_id = ? AND processed_flag = 0")
                        .bind(&meeting_id)
                        .fetch_one(db.pool()).await
                        .map(|r| r.get::<i64, _>("cnt"))
                        .unwrap_or(0);

                    if pending_chunks > 0 {
                        // 放回 pending，等 chunk_summary 完成；超过 MAX 次改为 failed，防止死循环
                        let _ = sqlx::query("UPDATE jobs SET status = CASE WHEN attempts + 1 >= ? THEN 'failed' ELSE 'pending' END, updated_at = datetime('now'), attempts = attempts + 1 WHERE id = ?")
                            .bind(MAX_JOB_ATTEMPTS)
                            .bind(&job_id)
                            .execute(db.pool()).await;
                        continue;
                    }

                    // 获取所有 chunk 文本（优先用标点恢复后的文本）
                    // 2026-09-04: 精转已移除，纪要只用快转段（conf<0.95）。
                    // 兼容旧数据：若存在精转段（conf>=0.95），则只取精转段（旧流程的最终结果）
                    let chunks = sqlx::query("SELECT transcript, punctuated_text, speaker, confidence FROM chunks WHERE meeting_id = ? ORDER BY start_time ASC")
                        .bind(&meeting_id)
                        .fetch_all(db.pool()).await
                        .unwrap_or_default();

                    let has_final_chunks = chunks.iter().any(|r| r.get::<f64, _>("confidence") >= 0.95);
                    let chunks: Vec<_> = chunks.into_iter().filter(|r| {
                        let conf: f64 = r.get("confidence");
                        if has_final_chunks { conf >= 0.95 } else { conf < 0.95 }
                    }).collect();

                    // v2.4.11: 空转写守卫——无任何有效转写文本时禁止生成纪要。
                    // 根因：麦克风权限被拒时 cpal 采集全零 → 静音段不入库（9-11 防幻觉），
                    // chunks 为空时旧代码仍把空输入喂给 LLM → 模型凭空编造整篇纪要并落库
                    // （9-24 实测：35 段全静音，纪要却生成"张三/李四/王五"虚构内容）。
                    let has_any_text = chunks.iter().any(|r| {
                        let t: String = r.get("transcript");
                        !t.trim().is_empty()
                    });
                    if !has_any_text {
                        eprintln!("[Worker] aggregate 拒绝：会议 {} 无有效转写文本（chunks={}），不生成纪要", meeting_id, chunks.len());
                        let _ = sqlx::query("UPDATE jobs SET status = 'failed', updated_at = datetime('now') WHERE id = ?")
                            .bind(&job_id)
                            .execute(db.pool()).await;
                        let _ = app_handle.emit(&event_name, serde_json::json!({
                            "type": "final_summary_failed",
                            "meeting_id": meeting_id,
                            "reason": "无有效转写内容（可能麦克风未采集到声音），请检查录音后再生成纪要",
                        }));
                        let _ = app_handle.emit("summary_progress", serde_json::json!({
                            "meeting_id": meeting_id,
                            "phase": "failed",
                        }));
                        continue;
                    }

                    // P4: record 模板走"手工记录式"纪要
                    // 1. 代码层：同说话人相邻段合并 + 补标点
                    // 2. 本地4B：按说话人组织、按话题分段、适度理顺（不逐字、不加额外结构）
                    if template_type == "record" {
                        let sherpa_url = std::env::var("BIJIAN_SHERPA_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

                        // --- 第一步：代码层预处理 ---
                        // 补标点 + 按说话人合并相邻段
                        let mut merged_lines: Vec<(String, String)> = Vec::new(); // (speaker, text)
                        for r in &chunks {
                            let raw: String = r.get("transcript");
                            let punctuated: Option<String> = r.try_get("punctuated_text").ok();
                            let speaker: String = r.get("speaker");

                            // P5 兜底：如果 punctuated_text 为空，用 ct-transformer 补标点
                            let display_text = if let Some(p) = punctuated {
                                if p.is_empty() {
                                    match http_client.post(format!("{}/punctuate", sherpa_url))
                                        .json(&serde_json::json!({"text": raw}))
                                        .timeout(std::time::Duration::from_secs(15))
                                        .send().await
                                    {
                                        Ok(resp) => {
                                            let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                            result.get("text").and_then(|t| t.as_str()).unwrap_or(&raw).to_string()
                                        }
                                        Err(_) => raw.clone(),
                                    }
                                } else {
                                    p
                                }
                            } else {
                                match http_client.post(format!("{}/punctuate", sherpa_url))
                                    .json(&serde_json::json!({"text": raw}))
                                    .timeout(std::time::Duration::from_secs(15))
                                    .send().await
                                {
                                    Ok(resp) => {
                                        let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                        result.get("text").and_then(|t| t.as_str()).unwrap_or(&raw).to_string()
                                    }
                                    Err(_) => raw.clone(),
                                }
                            };

                            let speaker_label = if speaker == "unknown" || speaker.is_empty() {
                                "发言".to_string()
                            } else {
                                speaker.clone()
                            };

                            // 合并同说话人相邻段
                            if let Some(last) = merged_lines.last_mut() {
                                if last.0 == speaker_label {
                                    last.1.push_str(&display_text);
                                    continue;
                                }
                            }
                            merged_lines.push((speaker_label, display_text));
                        }

                        // --- 第二步：本地4B模型轻结构化（手工记录式）---
                        let record_input = merged_lines.iter()
                            .map(|(sp, text)| format!("[{}]: {}", sp, text))
                            .collect::<Vec<_>>()
                            .join("\n\n");

                        // 截取防止超长
                        let record_input = crate::services::safe_truncate(&record_input, 8000).to_string();

                        let record_prompt = format!(
                            "/no_think\n你是一个会议记录助手。以下是一段会议的转写文本（已按说话人标注）。\n\n\
                            请按照手工记录的方式整理：\n\
                            0. 第一行先输出【概要】+一句话总结本场会议核心内容和目的（50字内，不许只写段数）\n\
                            1. 按说话人组织，每人一个大段\n\
                            2. 每人内部按话题分段，话题转换时自然分段\n\
                            3. 保留关键观点和决策，去掉重复和寒暄\n\
                            4. 像自己边听边记一样，按理解适度梳理，不要逐字记录\n\
                            5. 不加额外结构（如会议要点、结论、待办等）\n\
                            6. 不改变原意，不增加原文没有的信息\n\n\
                            转写文本：\n{}",
                            record_input
                        );

                        let record_payload = serde_json::json!({
                            "model": model,
                            "messages": [{"role": "user", "content": record_prompt}],
                            "stream": false,
                            "options": {"num_ctx": 8192, "num_predict": 4096}
                        });

                        // v2.4.8: 回退拼接提取为闭包 + HTTP 状态检查（此前 400 走 Ok 分支产出空内容）
                        let record_fallback = || merged_lines.iter()
                            .map(|(sp, text)| format!("**{}:** {}", sp, text))
                            .collect::<Vec<_>>()
                            .join("\n\n");

                        let record_result = match http_client.post(format!("{}/api/chat", ollama_url))
                            .json(&record_payload)
                            .timeout(std::time::Duration::from_secs(180))
                            .send().await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                let s = result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string();
                                if s.trim().is_empty() {
                                    eprintln!("[Worker] record纪要LLM返回空内容，回退到逐段拼接");
                                    record_fallback()
                                } else {
                                    s
                                }
                            }
                            Ok(resp) => {
                                eprintln!("[Worker] record纪要LLM返回异常状态 {}，回退到逐段拼接", resp.status());
                                record_fallback()
                            }
                            Err(e) => {
                                eprintln!("[Worker] record纪要LLM生成失败，回退到逐段拼接: {}", e);
                                record_fallback()
                            }
                        };

                        // v2.4.7: 从 record 输出解析【概要】行作为 tldr——
                        // 此前 tldr 是代码占位符"共 N 段发言"，毫无信息量
                        let record_tldr = record_result.lines()
                            .find(|l| l.trim_start().starts_with("【概要】"))
                            .map(|l| l.trim_start().trim_start_matches("【概要】").trim().to_string())
                            .filter(|s| !s.is_empty())
                            .unwrap_or_else(|| format!("共 {} 段发言", merged_lines.len()));

                        let record_content = serde_json::json!({
                            "record": true,
                            "content": record_result,
                            "lines": merged_lines.iter()
                                .map(|(sp, text)| format!("**{}:** {}", sp, text))
                                .collect::<Vec<_>>(),
                            "tldr": record_tldr,
                            "partial_note": partial_note,
                        }).to_string();

                        let sid = Uuid::new_v4().to_string();
                        let _ = sqlx::query("INSERT INTO final_summaries (id, meeting_id, content, summary_type) VALUES (?, ?, ?, 'record')")
                            .bind(&sid).bind(&meeting_id).bind(&record_content)
                            .execute(db.pool()).await;

                        let _ = sqlx::query("UPDATE meetings SET status = 'completed', updated_at = datetime('now') WHERE id = ?")
                            .bind(&meeting_id)
                            .execute(db.pool()).await;

                        let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                            .bind(&job_id)
                            .execute(db.pool()).await;

                        let _ = app_handle.emit(&event_name, serde_json::json!({"type": "final_summary_ready", "meeting_id": meeting_id}));
                        continue;
                    }

                    // v2.4.0: agenda 模板走"议程结构式"纪要
                    // 解析 depth 参数（提前到此处，agenda fallback 到 record 时需要）
                    let depth = payload_val.get("depth").and_then(|d| d.as_str()).unwrap_or("standard");

                    if template_type == "agenda" {
                        use crate::agenda_detect as ad;

                        // --- 第一步：构建 chunks (start_time, text) ---
                        let mut time_chunks: Vec<(f64, String)> = Vec::new();
                        for r in &chunks {
                            let start_time: f64 = r.try_get("start_time").unwrap_or(0.0);
                            let text: String = r.get("transcript");
                            time_chunks.push((start_time, text));
                        }

                        // --- 议程检测 ---
                        let anchors = ad::detect_agendas(&time_chunks);
                        if anchors.len() < 3 {
                            // 锚点不足，fallback 到 record 路径
                            let _ = sqlx::query("UPDATE jobs SET status = 'pending', payload = ? WHERE id = ?")
                                .bind(serde_json::json!({"template_type": "record", "depth": depth}).to_string())
                                .bind(&job_id)
                                .execute(db.pool()).await;
                            continue;
                        }

                        let segments = ad::build_segments(&anchors, &time_chunks);
                        let meeting_title: String = sqlx::query("SELECT title FROM meetings WHERE id = ?")
                            .bind(&meeting_id)
                            .fetch_optional(db.pool()).await
                            .ok().flatten()
                            .and_then(|r| r.try_get("title").ok())
                            .unwrap_or_else(|| "会议".to_string());
                        let skeleton = ad::build_skeleton(&segments, &meeting_title);

                        // 通知前端：议程检测完成
                        let _ = app_handle.emit("summary_progress", serde_json::json!({
                            "type": "agenda_detected",
                            "segments": segments.len(),
                            "anchors": anchors.len(),
                        }));

                        // --- 第二步：逐段压缩 ---
                        let mut seg_texts: Vec<String> = vec![String::new(); segments.len()];
                        let _sherpa_url = std::env::var("BIJIAN_SHERPA_URL")
                            .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

                        for (i, seg) in segments.iter().enumerate() {
                            // 提取段文本
                            let seg_text: String = time_chunks.iter()
                                .filter(|(t, _)| *t >= seg.t0 && *t < seg.t1)
                                .map(|(_, txt)| txt.as_str())
                                .collect::<Vec<_>>().join("");
                            if seg_text.trim().is_empty() { continue; }

                            // 2000 字切块
                            let chunk_size = 2000usize;
                            let overlap = 200usize;
                            let mut blocks: Vec<String> = Vec::new();
                            let mut idx = 0;
                            while idx < seg_text.len() {
                                let end = (idx + chunk_size).min(seg_text.len());
                                blocks.push(seg_text[idx..end].to_string());
                                if end >= seg_text.len() { break; }
                                idx = end - overlap;
                            }

                            // 块摘要
                            let mut block_summaries: Vec<String> = Vec::new();
                            for blk in &blocks {
                                let prompt = format!(
                                    "/no_think 以下是一段会议转写文本（环节：{}）。\n\
                                    请将其压缩为 400 字左右的摘要，保留：发言人观点、具体做法、数据、决策。\
                                    去掉口语重复和寒暄。直接输出摘要正文，不要任何标题或开头引导语。\n\
                                    只整理提供的转写内容，禁止添加、推测或补充材料中没有的信息。\n\n转写文本：\n{}",
                                    seg.person, blk
                                );
                                let payload = serde_json::json!({
                                    "model": model,
                                    "messages": [{"role": "user", "content": prompt}],
                                    "stream": false,
                                    "options": {"num_predict": 500, "num_ctx": 8192}
                                });
                                let result = match http_client.post(format!("{}/api/chat", ollama_url))
                                    .json(&payload)
                                    .timeout(std::time::Duration::from_secs(120))
                                    .send().await
                                {
                                    Ok(resp) => {
                                        let r: serde_json::Value = resp.json().await.unwrap_or_default();
                                        r.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string()
                                    }
                                    Err(e) => {
                                        eprintln!("[Worker] agenda 块摘要失败: {}", e);
                                        blk.chars().take(400).collect()
                                    }
                                };
                                block_summaries.push(result);
                                // 块间微歇（让 router 智能路由管理热）
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            }

                            // 段摘要（合并块摘要）
                            let merged = block_summaries.join("\n\n");
                            let prompt = format!(
                                "/no_think 以下是环节\"{}\"的分块摘要。请合并为一份 700 字左右的环节摘要，\
                                保留主要观点、关键数据和结论，语言简洁。直接输出正文，不要标题和开头引导语。\n\n分块摘要：\n{}",
                                seg.person, merged
                            );
                            let payload = serde_json::json!({
                                "model": model,
                                "messages": [{"role": "user", "content": prompt}],
                                "stream": false,
                                "options": {"num_predict": 1000, "num_ctx": 8192}
                            });
                            let seg_text_result = match http_client.post(format!("{}/api/chat", ollama_url))
                                .json(&payload)
                                .timeout(std::time::Duration::from_secs(120))
                                .send().await
                            {
                                Ok(resp) => {
                                    let r: serde_json::Value = resp.json().await.unwrap_or_default();
                                    r.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string()
                                }
                                Err(e) => {
                                    eprintln!("[Worker] agenda 段摘要失败: {}", e);
                                    merged.chars().take(700).collect()
                                }
                            };
                            seg_texts[i] = seg_text_result;

                            let _ = app_handle.emit("summary_progress", serde_json::json!({
                                "type": "agenda_segment_done",
                                "segment": i + 1,
                                "total": segments.len(),
                                "person": seg.person.clone(),
                            }));
                        }

                        // --- 第三步：骨架拼接（零 LLM） ---
                        let final_text = ad::assemble(&skeleton, &seg_texts);
                        // 人名兜底替换
                        let final_text = final_text
                            .replace("保密总", "宝玉总")
                            .replace("保育总", "宝玉总")
                            .replace("鲍玉总", "宝玉总");
                        // 剥 markdown 符号
                        let final_text = final_text
                            .replace("**", "");
                        let final_text: String = final_text.lines()
                            .filter(|l| l.trim() != "---")
                            .collect::<Vec<_>>().join("\n");

                        // 结构校验
                        let (ok, _missing) = ad::validate_structure(&final_text, &skeleton);
                        if !ok {
                            eprintln!("[Worker] agenda 结构校验: 缺失 {:?}", _missing);
                        }

                        let record_content = serde_json::json!({
                            "record": true,
                            "content": final_text,
                            "tldr": "议程结构式纪要（自动检测+骨架拼接）",
                            "partial_note": partial_note,
                        }).to_string();

                        let sid = Uuid::new_v4().to_string();
                        let _ = sqlx::query("INSERT INTO final_summaries (id, meeting_id, content, summary_type) VALUES (?, ?, ?, 'record')")
                            .bind(&sid).bind(&meeting_id).bind(&record_content)
                            .execute(db.pool()).await;

                        let _ = sqlx::query("UPDATE meetings SET status = 'completed', updated_at = datetime('now') WHERE id = ?")
                            .bind(&meeting_id)
                            .execute(db.pool()).await;

                        let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                            .bind(&job_id)
                            .execute(db.pool()).await;

                        let _ = app_handle.emit(&event_name, serde_json::json!({"type": "final_summary_ready", "meeting_id": meeting_id}));
                        continue;
                    }

                    // depth 已在 agenda 块前解析

                    let sherpa_url = std::env::var("BIJIAN_SHERPA_URL")
                        .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());

                    // 纪要开始事件
                    let _ = app_handle.emit("summary_progress", serde_json::json!({
                        "meeting_id": meeting_id,
                        "phase": "start",
                    }));

                    let full_text: String = chunks.iter()
                        .map(|r| {
                            let raw: String = r.get("transcript");
                            let punctuated: Option<String> = r.try_get("punctuated_text").ok();
                            let speaker: String = r.get("speaker");
                            let text = punctuated.filter(|s| !s.is_empty()).unwrap_or(raw);
                            format!("[{}]: {}", speaker, text)
                        })
                        .collect::<Vec<_>>().join("\n\n");

                    // P5 兜底：如果全量文本没有任何标点，统一调一次 /punctuate
                    let full_text = if !full_text.contains('，') && !full_text.contains('。') && !full_text.contains('？') && !full_text.contains('！') {
                        match http_client.post(format!("{}/punctuate", sherpa_url))
                            .json(&serde_json::json!({"text": full_text}))
                            .timeout(std::time::Duration::from_secs(30))
                            .send().await
                        {
                            Ok(resp) => {
                                let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                result.get("text").and_then(|t| t.as_str()).unwrap_or(&full_text).to_string()
                            }
                            Err(_) => full_text,
                        }
                    } else {
                        full_text
                    };

                    // Map-Reduce 分片摘要：长文本分片独立摘要，再合并
                    // v2.4.10: 分片 800→2000 字（片数 20→10，总时间减半）；重叠 80→200 保连贯
                    const CHUNK_CHAR_LIMIT: usize = 2000;
                    const CHUNK_OVERLAP: usize = 200;
                    let total_chars = full_text.chars().count();

                    let content = if total_chars > CHUNK_CHAR_LIMIT {
                        // === Map-Reduce 模式 ===
                        let chunk_texts: Vec<String> = {
                            let mut result = Vec::new();
                            let chars: Vec<char> = full_text.chars().collect();
                            let mut i = 0;
                            while i < chars.len() {
                                let end = (i + CHUNK_CHAR_LIMIT).min(chars.len());
                                result.push(chars[i..end].iter().collect());
                                if end >= chars.len() { break; }
                                i = end.saturating_sub(CHUNK_OVERLAP);
                            }
                            result
                        };
                        let total_chunks = chunk_texts.len();

                        // Map: 分片摘要（v2.4.8: 2 路并发——GPU/CPU 双管道同时吃，
                        // 串行 20 片曾是 20 分钟瓶颈；单片失败跳过不中断全局）
                        let mut chunk_summaries: Vec<String> = vec![String::new(); total_chunks];
                        let mut failed_chunks: Vec<usize> = Vec::new();
                        let done_counter = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
                        let sem = std::sync::Arc::new(tokio::sync::Semaphore::new(2));

                        let mut map_futs = Vec::with_capacity(total_chunks);
                        for (idx, chunk_text) in chunk_texts.iter().enumerate() {
                            let http_client = http_client.clone();
                            let ollama_url = ollama_url.clone();
                            let model = model.clone();
                            let app_handle = app_handle.clone();
                            let meeting_id = meeting_id.clone();
                            let done_counter = done_counter.clone();
                            let chunk_text: String = chunk_text.clone();
                            let sem = sem.clone();
                            let total = total_chunks;
                            map_futs.push(async move {
                                let _permit = sem.acquire().await;
                                let n = done_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                                let _ = app_handle.emit("summary_progress", serde_json::json!({
                                    "meeting_id": meeting_id,
                                    "phase": "mapping",
                                    "current": n,
                                    "total": total,
                                }));

                                let map_prompt = format!(
                                    "/no_think\n以下是会议转写内容的第 {}/{} 部分。请提取这部分的核心要点（议题、讨论要点、决策、行动项），保持数据和关键信息原样，以简洁的条目列出，不要遗漏重要内容。\n\n转写内容：\n{}",
                                    idx + 1, total, chunk_text
                                );

                                let map_payload = serde_json::json!({
                                    "model": model,
                                    "messages": [{"role": "user", "content": map_prompt}],
                                    "stream": false,
                                    "options": {"num_ctx": 4096, "num_predict": 600}
                                });

                                let outcome = match http_client.post(format!("{}/api/chat", ollama_url))
                                    .json(&map_payload)
                                    .timeout(std::time::Duration::from_secs(240))
                                    .send().await
                                {
                                    Ok(resp) if resp.status().is_success() => {
                                        let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                        Ok(result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string())
                                    }
                                    Ok(resp) => Err(format!("HTTP {}", resp.status())),
                                    Err(e) => Err(e.to_string()),
                                };
                                (idx, outcome)
                            });
                        }

                        for (idx, outcome) in futures::future::join_all(map_futs).await {
                            match outcome {
                                Ok(s) if !s.trim().is_empty() => chunk_summaries[idx] = s,
                                Ok(_) => {
                                    eprintln!("[Worker] map分片{}摘要返回空，跳过", idx + 1);
                                    failed_chunks.push(idx + 1);
                                }
                                Err(e) => {
                                    eprintln!("[Worker] map分片{}摘要失败: {}，跳过", idx + 1, e);
                                    failed_chunks.push(idx + 1);
                                }
                            }
                        }

                        // 如果有失败的分片，在 reduce 文本中标注
                        let merge_input = if !failed_chunks.is_empty() {
                            format!("注意：第 {} 片转写处理失败，内容可能不完整。\n\n{}",
                                failed_chunks.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("、"),
                                chunk_summaries.join("\n\n---\n\n"))
                        } else {
                            chunk_summaries.join("\n\n---\n\n")
                        };

                        // Reduce: 合并摘要生成最终纪要
                        let _ = app_handle.emit("summary_progress", serde_json::json!({
                            "meeting_id": meeting_id,
                            "phase": "reducing",
                        }));

                        let prompt = build_template_prompt(template_type, &merge_input, depth);

                        // v2.4.8: reduce 上下文自适应——此前 num_ctx=4096 固定，
                        // 20 片 map 摘要拼出 18547 字曾把 4k ctx 撑爆 → Ollama 400 → 空纪要落库
                        let merge_chars = merge_input.chars().count();
                        let est_tokens = (merge_chars as f64 * 0.8).ceil() as usize + 2048;
                        let reduce_ctx = est_tokens.clamp(4096, 16384);

                        let payload = serde_json::json!({
                            "model": model,
                            "messages": [{"role": "user", "content": prompt}],
                            "stream": false,
                            "format": "json",
                            "options": {"num_ctx": reduce_ctx, "num_predict": 2048}
                        });

                        match http_client.post(format!("{}/api/chat", ollama_url))
                            .json(&payload)
                            .timeout(std::time::Duration::from_secs(900))
                            .send().await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string()
                            }
                            Ok(resp) => {
                                eprintln!("[Worker] reduce纪要LLM返回异常状态 {} (num_ctx={})", resp.status(), reduce_ctx);
                                "{\"tldr\": \"纪要生成失败\", \"topics\": [], \"decisions\": [], \"actions\": [], \"questions\": [], \"key_points\": []}".to_string()
                            }
                            Err(e) => {
                                eprintln!("[Worker] reduce纪要生成失败: {}", e);
                                "{\"tldr\": \"纪要生成失败\", \"topics\": [], \"decisions\": [], \"actions\": [], \"questions\": [], \"key_points\": []}".to_string()
                            }
                        }
                    } else {
                        // === 单次模式（文本较短，直接处理）===
                        let prompt = build_template_prompt(template_type, &full_text, depth);

                        let payload = serde_json::json!({
                            "model": model,
                            "messages": [{"role": "user", "content": prompt}],
                            "stream": false,
                            "format": "json",
                            "options": {"num_ctx": 4096, "num_predict": 2048}
                        });

                        match http_client.post(format!("{}/api/chat", ollama_url))
                            .json(&payload)
                            .timeout(std::time::Duration::from_secs(480))
                            .send().await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").to_string()
                            }
                            Ok(resp) => {
                                eprintln!("[Worker] 单次模式LLM返回异常状态 {}", resp.status());
                                "{\"tldr\": \"纪要生成失败\", \"topics\": [], \"decisions\": [], \"actions\": [], \"questions\": [], \"key_points\": []}".to_string()
                            }
                            Err(e) => {
                                eprintln!("[Worker] 单次纪要生成失败: {}", e);
                                "{\"tldr\": \"纪要生成失败\", \"topics\": [], \"decisions\": [], \"actions\": [], \"questions\": [], \"key_points\": []}".to_string()
                            }
                        }
                    };

                    // 剥离 markdown 围栏（```json ... ```）
                    let content = {
                        let trimmed = content.trim();
                        if trimmed.starts_with("```") {
                            let inner: String = trimmed.lines().skip(1).collect::<Vec<_>>().join("\n");
                            inner.trim_end_matches("```").trim().to_string()
                        } else {
                            content
                        }
                    };

                    // v2.5.2 部分转写标注注入：final 纪要是合法 JSON 时追加 partial_note 字段（前端渲染头部警告条）；
                    // 非合法 JSON（LLM 偶发 markdown 输出）时原样保留不注入——不为标注引入落库拒绝的新风险
                    let content = {
                        if !partial_note.is_empty() {
                            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&content) {
                                v["partial_note"] = serde_json::json!(partial_note);
                                v.to_string()
                            } else {
                                content // 原样保留（不注入、不拒绝）
                            }
                        } else {
                            content
                        }
                    };

                    // v2.4.8: 空输出/失败防落库——空记录会遮住旧纪要且误导用户
                    if content.trim().is_empty() || content.contains("纪要生成失败") {
                        eprintln!("[Worker] reduce 纪要生成失败（空/异常），不落库，job 标记 failed");
                        let _ = sqlx::query("UPDATE jobs SET status = 'failed', updated_at = datetime('now') WHERE id = ?")
                            .bind(&job_id).execute(db.pool()).await;
                        let _ = app_handle.emit(&event_name, serde_json::json!({
                            "type": "final_summary_failed",
                            "meeting_id": meeting_id,
                            "reason": "reduce 输出为空或 LLM 返回异常",
                        }));
                        let _ = app_handle.emit("summary_progress", serde_json::json!({
                            "meeting_id": meeting_id,
                            "phase": "failed",
                        }));
                        continue;
                    }

                    let sid = Uuid::new_v4().to_string();
                    let _ = sqlx::query("INSERT INTO final_summaries (id, meeting_id, content, summary_type) VALUES (?, ?, ?, 'final')")
                        .bind(&sid).bind(&meeting_id).bind(&content)
                        .execute(db.pool()).await;

                    let _ = sqlx::query("UPDATE meetings SET status = 'completed', updated_at = datetime('now') WHERE id = ?")
                        .bind(&meeting_id)
                        .execute(db.pool()).await;

                    let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                        .bind(&job_id)
                        .execute(db.pool()).await;

                    let _ = app_handle.emit(&event_name, serde_json::json!({"type": "final_summary_ready", "meeting_id": meeting_id}));

                } else if job_type == "correction" {
                    // P2: 本地 LLM 文本纠错 — 逐段用 Ollama qwen3-4b + 热词表修正 ASR 输出
                    let meeting_title: String = sqlx::query("SELECT title FROM meetings WHERE id = ?")
                        .bind(&meeting_id)
                        .fetch_optional(db.pool()).await.ok().flatten()
                        .map(|r| r.try_get::<String, _>("title").unwrap_or_default())
                        .unwrap_or_default();

                    // 加载热词表（wrong → correct），构造成参照清单
                    let hw_rows = sqlx::query("SELECT wrong_text, correct_text FROM hotwords ORDER BY priority DESC, length(correct_text) DESC LIMIT 50")
                        .fetch_all(db.pool()).await.unwrap_or_default();
                    let hotword_list: Vec<(String, String)> = hw_rows.iter().map(|r| {
                        (r.get::<String, _>("wrong_text"), r.get::<String, _>("correct_text"))
                    }).collect();

                    // 取需要纠错的 chunks（跳过已纠错的 processed_flag=2）
                    let chunks_to_fix = sqlx::query("SELECT id, transcript FROM chunks WHERE meeting_id = ? AND (processed_flag IS NULL OR processed_flag < 2) ORDER BY start_time ASC")
                        .bind(&meeting_id)
                        .fetch_all(db.pool()).await.unwrap_or_default();

                    let total = chunks_to_fix.len();
                    if total == 0 {
                        let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                            .bind(&job_id).execute(db.pool()).await;
                        let _ = app_handle.emit(&event_name, serde_json::json!({"type": "correction_done", "meeting_id": meeting_id, "corrected": 0}));
                        // P4: TTL 清理
                        let _ = cleanup_old_recordings().await;
                        continue;
                    }

                    // 纠错开始事件
                    let _ = app_handle.emit("correction_progress", serde_json::json!({
                        "meeting_id": meeting_id,
                        "phase": "start",
                        "total": total,
                    }));

                    let mut corrected_count = 0u32;
                    for (idx, chunk_row) in chunks_to_fix.iter().enumerate() {
                        let chunk_id: String = chunk_row.get("id");
                        let original: String = chunk_row.get("transcript");

                        if original.is_empty() { continue; }

                        // 构造热词参照字符串
                        let hw_ref = if hotword_list.is_empty() {
                            String::new()
                        } else {
                            let lines: Vec<String> = hotword_list.iter()
                                .map(|(w, c)| format!("「{}」→「{}」", w, c))
                                .collect();
                            format!("\n\n参照纠正表（ASR 可能输出错误词 → 正确词）：\n{}", lines.join("\n"))
                        };

                        let prompt = format!(
                            "/no_think\n你是 ASR 文本纠错助手。下面是一段语音识别（ASR）的转写文本，可能包含同音字错误、专有名词错误、数字格式错误等。\n\n请根据以下规则纠错：\n1. 参照纠正表修正已知错误\n2. 修正明显的同音字/近音字错误\n3. 修正专有名词（如公司名、产品名、人名）\n4. 数字格式统一（「百分之十五」→「15%」，「三万」→「3万」）\n5. 不要改变原意，不要增删内容，不要润色，只做最小纠错\n6. 输出纯文本，不要加任何解释、标注或格式标记\n{}\n\n会议标题：{}\n转写文本：\n{}",
                            hw_ref, meeting_title, original
                        );

                        let payload = serde_json::json!({
                            "model": model,
                            "messages": [{"role": "user", "content": prompt}],
                            "stream": false,
                            "options": {"num_ctx": 2048, "num_predict": 1024, "temperature": 0.1}
                        });

                        let corrected = match http_client.post(format!("{}/api/chat", ollama_url))
                            .json(&payload)
                            .timeout(std::time::Duration::from_secs(60))
                            .send().await
                        {
                            Ok(resp) => {
                                let result: serde_json::Value = resp.json().await.unwrap_or_default();
                                result.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()).unwrap_or("").trim().to_string()
                            }
                            Err(e) => {
                                eprintln!("[Worker] 纠错失败 chunk {}: {}", chunk_id, e);
                                original.clone() // 失败保留原文
                            }
                        };

                        // 只在纠错结果与原文不同时更新
                        if corrected != original && !corrected.is_empty() {
                            let _ = sqlx::query("UPDATE chunks SET transcript = ?, word_count = ?, processed_flag = 2 WHERE id = ?")
                                .bind(&corrected).bind(corrected.len() as i64).bind(&chunk_id)
                                .execute(db.pool()).await;
                            corrected_count += 1;
                        } else {
                            // 无变化也标记为已纠错
                            let _ = sqlx::query("UPDATE chunks SET processed_flag = 2 WHERE id = ?")
                                .bind(&chunk_id)
                                .execute(db.pool()).await;
                        }

                        // 进度事件（每 5 段或最后一段）
                        if idx % 5 == 0 || idx == total - 1 {
                            let _ = app_handle.emit("correction_progress", serde_json::json!({
                                "meeting_id": meeting_id,
                                "phase": "processing",
                                "current": idx + 1,
                                "total": total,
                            }));
                        }

                        // 资源调度：段间间隔 1 秒
                        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    }

                    let _ = app_handle.emit("correction_progress", serde_json::json!({
                        "meeting_id": meeting_id,
                        "phase": "done",
                        "corrected": corrected_count,
                        "total": total,
                    }));

                    let _ = app_handle.emit(&event_name, serde_json::json!({
                        "type": "correction_done",
                        "meeting_id": meeting_id,
                        "corrected": corrected_count,
                    }));

                    let _ = sqlx::query("UPDATE jobs SET status = 'done', updated_at = datetime('now') WHERE id = ?")
                        .bind(&job_id).execute(db.pool()).await;
                }

                // P4: TTL 清理 — 删除 7 天以上的临时录音文件
                let _ = cleanup_old_recordings().await;
            }
        });
    }

    /// P4: 清理 7 天以上的临时录音目录
    async fn cleanup_old_recordings() {
        let audio_dir = format!("{}/Library/Application Support/笔尖/audio_cache", std::env::var("HOME").unwrap_or_default());
        let cutoff = chrono::Utc::now().timestamp() - 7 * 24 * 3600; // 7天前

        if let Ok(mut entries) = tokio::fs::read_dir(&audio_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                // 录音目录格式: rec_TIMESTAMP
                if let Some(ts_str) = name.strip_prefix("rec_") {
                    if let Ok(ts) = ts_str.parse::<i64>() {
                        if ts < cutoff {
                            let _ = tokio::fs::remove_dir_all(entry.path()).await;
                            eprintln!("[Cleanup] 删除过期录音目录: {}", name);
                        }
                    }
                }
            }
        }
    }

    fn j_get(row: &sqlx::sqlite::SqliteRow, field: &str) -> String {
        use sqlx::Row;
        row.try_get::<String, _>(field).unwrap_or_default()
    }

    fn build_template_prompt(template: &str, full_text: &str, depth: &str) -> String {
        // 深度档位决定输出结构和详细程度
        // v2.4.7: 全档位新增「密度三律」——数字钉死、错词修正、禁复述语气
        let (base_json, extra_instruction) = match depth {
            "brief" => (
                r#"输出 JSON 格式：
{"tldr": "一句话概要(50字内)", "key_points": ["3-5条核心结论"], "actions": [{"text": "行动项", "assignee": "负责人"}]}"#,
                "精简版：只提取核心结论和行动项，保持简洁，不要冗长。会议中的关键数字必须保留。"
            ),
            "detailed" => (
                r#"输出 JSON 格式：
{"tldr": "一句话概要", "topics": [{"title": "主题", "summary": "2-4句详细要点(含关键数据和上下文)", "key_data": ["涉及的数字/金额/比例等"]}], "decisions": [{"text": "决策内容(以'决定XXX'陈述)", "context": "决策背景"}], "actions": [{"text": "行动项(动词开头)", "assignee": "负责人", "deadline": "截止日期(如有)"}], "questions": ["待决问题(问句形式)"], "uncertainties": ["转写中模糊或不确定的数字/名字等"], "speaker_notes": [{"speaker": "说话人", "key_points": ["关键发言要点"]}]}"#,
                "详细版：保留细节和数据，每个议题写2-4句详细要点，决策需附背景，行动项需含截止日期，不确定信息标注为待确认。key_data 必须穷举该主题涉及的所有关键数字（金额、比例、期限、数量），宁多勿漏。"
            ),
            _ => (
                // 标准版（默认）
                r#"输出 JSON 格式：
{"tldr": "一句话概要", "topics": [{"title": "议题", "summary": "2-4句要点(不要只写标题)"}], "decisions": ["决策项(以'决定XXX'陈述)"], "actions": [{"text": "行动项(动词开头)", "assignee": "负责人"}], "questions": ["待决问题(问句形式)"], "key_points": ["关键结论"]}"#,
                "标准版：每个议题写2-4句要点（不超过4句，写实质内容不写套话），决策以事实陈述，行动项动词开头+明确负责人。会议中提到的数字、金额、比例必须原样保留，禁止四舍五入或省略。"
            )
        };

        let template_instruction = match template {
            "interview" => "请根据以下访谈/招聘转写内容生成结构化纪要。重点关注：受访者观点、关键洞察、受访者和访谈者互动要点。招聘场景请记录：候选人背景、专业能力评价、薪资期望、面试官结论。",
            "training" => "请根据以下培训/警示教育转写内容生成结构化纪要。此类会议为单一主讲人的长篇讲授，请按以下骨架组织：\n1. 培训主题与主讲人要旨（为什么讲、针对什么问题）\n2. 分主题知识点：主讲人划分的每个大类（如风险类别、专题模块）独立成 topic，每个 topic 写清：认定口径/核心要求、典型案例特征、关键数据（处罚力度、金额、比例必须钉出）\n3. 工作要求/整改部署：主讲人对听众提出的明确要求、流程改变、系统上线等行动指令，归入 actions\n4. 特别注意：转写中同音错词按上下文专业修正（如「低盐流量」→「低消流量」、「五甲」→「五假」、「IPT/ICP」→「ICT」），修正后照常使用。",
            "standup" => "请根据以下站会转写内容生成精简站会纪要。重点关注：每人汇报要点、阻塞问题、今日计划。保持简洁，不要冗长。",
            // v2.6.0 新增：客户拜访（对齐财务条线客户走访场景）
            "visit" => "请根据以下客户拜访转写内容生成结构化拜访纪要。按此骨架组织：\n1. 客户与背景：拜访对象单位/部门/接待人、拜访目的\n2. 客户诉求与痛点：客户提出的业务问题、对现有服务的不满或期待\n3. 我方回应与承诺：现场承诺事项、口径边界（不得超授权承诺的要点单列）\n4. 商务进展：涉及的合作金额、报价、合同状态、回款事项（数字/金额/时间原样保留）\n5. 待办与下一步：行动项动词开头+责任人+时限，客户侧与我方侧分列。",
            // v2.6.0 新增：项目评审（评审会决策导向）
            "project" => "请根据以下项目评审转写内容生成结构化评审纪要。按此骨架组织：\n1. 评审结论：通过/有条件通过/不通过，及其体结论依据\n2. 方案要点：被评审方案的核心内容归纳（不重复全文）\n3. 评审意见：各位评审人意见分点列出（保留评审人姓名），风险与问题单列\n4. 修改要求：对方案的明确修改项+责任人+时限\n5. 下一步节点：后续里程碑与时间安排（原样保留）。",
            "general" => "请根据以下转写内容生成结构化纪要。",
            _ => "请根据以下会议转写内容生成结构化会议纪要。重点关注：议题讨论、决策、行动项和责任人。",
        };

        // 去填充词指令：让模型在生成纪要时也过滤口语填充词
        let filler_instruction = "注意：转写内容中可能残留口语填充词（如「这个」「那个」「就是」「然后」「嗯」「啊」等无实义词汇），生成纪要时请自动忽略这些词，不要将它们写入输出。另外转写中的问号错标（如「？？」）多为语气词残留，一律忽略。";

        format!("/no_think\n{}\n{}\n{}\n{}\n\n转写内容：\n{}", template_instruction, extra_instruction, filler_instruction, base_json, full_text)
    }
}

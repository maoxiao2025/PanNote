// PanNote - Ollama 服务管理器
// PanNote 依赖本地 Ollama（127.0.0.1:11434）提供 LLM 能力。
// 本模块负责在应用启动时探测并自动拉起 Ollama，避免「打开 PanNote 但 Ollama 是关的」。
//
// 拉起策略（app 优先，无 app 再 serve）：
//   1) 优先 `open -a Ollama`（Ollama.app 独立常驻，PanNote 退出不影响它）
//   2) 若本机没装 Ollama.app，则退化 spawn `ollama serve` 子进程
//
// 复用 gpu_manager 的模式：健康检查 + 自动拉起 + keep-alive。

use std::process::{Command, Child};
use std::sync::{Mutex, atomic::{AtomicBool, AtomicI64, Ordering}};
use std::time::Duration;
use std::thread;

pub struct OllamaManager {
    serve_process: Mutex<Option<Child>>,
    starting: AtomicBool,
    ollama_url: String,
    /// 最近一次 LLM 请求的时间戳（Unix 秒），用于空闲卸载判定
    last_request_ts: AtomicI64,
}

impl OllamaManager {
    pub fn new() -> Self {
        Self {
            serve_process: Mutex::new(None),
            starting: AtomicBool::new(false),
            ollama_url: "http://127.0.0.1:11434".to_string(),
            last_request_ts: AtomicI64::new(0),
        }
    }

    fn log_dir() -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        format!("{}/Library/Application Support/PanNote/ollama_logs", home)
    }

    /// 探测 Ollama 是否已就绪（/api/tags 返回 200 视为可用）
    pub fn is_ollama_running(&self) -> bool {
        match ureq::get(&format!("{}/api/tags", self.ollama_url))
            .timeout(Duration::from_secs(3))
            .call()
        {
            Ok(resp) => resp.status() == 200,
            Err(_) => false,
        }
    }

    /// 确保 Ollama 在运行。未运行则自动拉起，并轮询等待就绪（最多约 30s）。
    /// 由调用方放到后台线程执行，避免阻塞 UI。
    pub fn ensure_running(&self) -> Result<(), String> {
        if self.is_ollama_running() {
            return Ok(());
        }
        if self.starting.swap(true, Ordering::SeqCst) {
            return Ok(()); // 已有线程在拉起中
        }

        log::info!("[Ollama Manager] Ollama 未运行，自动拉起...");

        let started = self.launch_via_app() || self.launch_via_serve();
        if !started {
            self.starting.store(false, Ordering::SeqCst);
            return Err("没有可用的 Ollama 启动方式（app 与 serve 均失败）".into());
        }

        // 轮询等待就绪（Ollama.app / serve 冷启动需数秒到数十秒）
        for _ in 0..30 {
            thread::sleep(Duration::from_secs(1));
            if self.is_ollama_running() {
                log::info!("[Ollama Manager] Ollama 已就绪");
                self.starting.store(false, Ordering::SeqCst);
                return Ok(());
            }
        }

        self.starting.store(false, Ordering::SeqCst);
        Err("Ollama 启动后 30s 内未就绪".into())
    }

    /// 方式 1：open -a Ollama（Ollama.app，独立常驻）
    fn launch_via_app(&self) -> bool {
        if !std::path::Path::new("/Applications/Ollama.app").exists() {
            return false;
        }
        match Command::new("open").args(["-a", "Ollama"]).status() {
            Ok(status) if status.success() => {
                log::info!("[Ollama Manager] 已通过 Ollama.app 拉起");
                true
            }
            _ => false,
        }
    }

    /// 方式 2：spawn `ollama serve` 子进程（无 Ollama.app 时的兜底）
    fn launch_via_serve(&self) -> bool {
        let Some(bin) = Self::find_ollama_bin() else {
            return false;
        };

        let log_dir = Self::log_dir();
        std::fs::create_dir_all(&log_dir).ok();
        let stdout_file = match std::fs::File::create(format!("{}/serve.log", log_dir)) {
            Ok(f) => f,
            Err(_) => return false,
        };
        let stderr_file = match std::fs::File::create(format!("{}/serve_err.log", log_dir)) {
            Ok(f) => f,
            Err(_) => return false,
        };

        match Command::new(&bin).arg("serve").stdout(stdout_file).stderr(stderr_file).spawn() {
            Ok(child) => {
                log::info!("[Ollama Manager] 已通过 `{} serve` 拉起 (PID {})", bin, child.id());
                if let Ok(mut proc) = self.serve_process.lock() {
                    *proc = Some(child);
                }
                true
            }
            Err(e) => {
                log::error!("[Ollama Manager] 启动 `{} serve` 失败: {}", bin, e);
                false
            }
        }
    }

    /// 定位 ollama 可执行文件（优先常见绝对路径，退回 PATH）
    fn find_ollama_bin() -> Option<String> {
        if let Ok(p) = std::env::var("OLLAMA_BIN") {
            if !p.is_empty() && std::path::Path::new(&p).exists() {
                return Some(p);
            }
        }
        for cand in ["/usr/local/bin/ollama", "/opt/homebrew/bin/ollama", "/usr/bin/ollama"] {
            if std::path::Path::new(cand).exists() {
                return Some(cand.to_string());
            }
        }
        None
    }

    /// 记录一次 LLM 请求（由 services.rs / commands.rs 调用）
    pub fn touch(&self) {
        self.last_request_ts.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
            Ordering::Relaxed,
        );
    }

    /// keep-alive 线程：定期探测 Ollama，挂了自动拉起；
    /// 同时空闲超过 2 分钟自动卸载模型（keep_alive=0），避免模型常驻占内存/发热。
    pub fn start_keepalive(self: std::sync::Arc<Self>) {
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(30));
                if !self.is_ollama_running() {
                    log::warn!("[Ollama Manager] Ollama 不在运行，尝试重新拉起...");
                    if let Err(e) = self.ensure_running() {
                        log::error!("[Ollama Manager] 重新拉起失败: {}", e);
                    }
                    continue;
                }

                // v2.4.5: 空闲卸载——最近一次请求超过 120 秒，发 keep_alive=0 释放模型
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                let last = self.last_request_ts.load(Ordering::Relaxed);
                if last > 0 && now - last > 120 {
                    // 检查 Ollama 是否有模型驻留
                    let need_unload = match ureq::get(&format!("{}/api/ps", self.ollama_url))
                        .timeout(Duration::from_secs(5))
                        .call()
                    {
                        Ok(resp) => {
                            let body: serde_json::Value =
                                serde_json::from_reader(resp.into_reader()).unwrap_or_default();
                            body.get("models")
                                .and_then(|m| m.as_array())
                                .map(|arr| !arr.is_empty())
                                .unwrap_or(false)
                        }
                        Err(_) => false,
                    };
                    if need_unload {
                        log::info!("[Ollama Manager] 空闲 {}s，卸载模型", now - last);
                        let _ = ureq::post(&format!("{}/api/generate", self.ollama_url))
                            .timeout(Duration::from_secs(10))
                            .send_string(
                                r#"{"model":"qwen3-4b-32k","keep_alive":0}"#,
                            );
                        // 标记已卸载，避免反复发
                        self.last_request_ts.store(0, Ordering::Relaxed);
                    }
                }
            }
        });
    }
}
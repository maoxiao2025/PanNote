// PanNote - GPU 服务管理器
// 管理 router v6 smart routing sidecar（Python）
// 复用 asr_manager 模式：进程 spawn + 健康检查 + 自动拉起
//
// router 监听 :11435，Ollama 兼容 API
// 自动分流：prompt密集→CPU / gen密集→GPU / 热谷→CPU散热

use std::process::{Command, Child};
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use std::thread;

pub struct GpuManager {
    router_process: Mutex<Option<Child>>,
    starting: AtomicBool,
    router_url: String,
    ollama_url: String,
    router_script: String,
    python_path: String,
}

impl GpuManager {
    pub fn new() -> Self {
        let project_dir = std::env::var("BIJIAN_PROJECT_DIR")
            .unwrap_or_else(|_| {
                format!("{}/Library/Application Support/PanNote/asr", std::env::var("HOME").unwrap_or_default())
            });

        let log_dir = Self::gpu_log_dir();
        std::fs::create_dir_all(&log_dir).ok();

        Self {
            router_process: Mutex::new(None),
            starting: AtomicBool::new(false),
            router_url: "http://127.0.0.1:11435".to_string(),
            ollama_url: "http://127.0.0.1:11434".to_string(),
            router_script: format!("{}/gpu/ollama-gpu-router.py", project_dir),
            python_path: std::env::var("BIJIAN_PYTHON")
                .unwrap_or_else(|_| {
                    "python3".to_string() // 产品版：系统 PATH 中的 python3
                }),
        }
    }

    fn gpu_log_dir() -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        format!("{}/Library/Application Support/PanNote/gpu_logs", home)
    }

    /// 检查 router 是否在运行
    pub fn is_router_running(&self) -> bool {
        if self.check_router_health() {
            return true;
        }
        if let Ok(mut proc) = self.router_process.lock() {
            if let Some(child) = proc.as_mut() {
                if let Ok(Some(_)) = child.try_wait() {
                    return false; // 已退出
                }
                return true;
            }
        }
        false
    }

    fn check_router_health(&self) -> bool {
        // router 兼容 Ollama API，用 /api/tags 做健康检查
        // v2.4.2: 超时 3s→8s——router 的 /api/tags 是透传到 Ollama，忙时 >3s 会误判
        // 不健康，进而触发 keepalive 反复拉起（昨日的 15h 崩溃循环 root cause）
        match ureq::get(&format!("{}/api/tags", self.router_url))
            .timeout(Duration::from_secs(8))
            .call()
        {
            Ok(resp) => resp.status() == 200,
            Err(_) => false,
        }
    }

    /// 获取 LLM URL：router 在线走 router（GPU 加速），否则走 Ollama 直连
    pub fn get_llm_url(&self) -> String {
        if self.check_router_health() {
            self.router_url.clone()
        } else {
            // fallback 到环境变量或默认 Ollama
            std::env::var("OLLAMA_URL")
                .unwrap_or_else(|_| self.ollama_url.clone())
        }
    }

    /// 启动 router sidecar
    pub fn start_router(&self) -> Result<(), String> {
        if self.starting.swap(true, Ordering::SeqCst) {
            return Ok(()); // 已经在启动中
        }

        // 如果已在运行，跳过
        if self.check_router_health() {
            self.starting.store(false, Ordering::SeqCst);
            return Ok(());
        }

        log::info!("[GPU Manager] 启动 router sidecar...");

        let log_dir = Self::gpu_log_dir();
        let stdout_file = std::fs::File::create(format!("{}/router.log", log_dir))
            .map_err(|e| format!("创建日志文件失败: {}", e))?;
        let stderr_file = std::fs::File::create(format!("{}/router_err.log", log_dir))
            .map_err(|e| format!("创建错误日志失败: {}", e))?;

        let child = Command::new(&self.python_path)
            .arg("-u")  // 无缓冲输出
            .arg(&self.router_script)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .spawn()
            .map_err(|e| {
                self.starting.store(false, Ordering::SeqCst);
                format!("启动 router 失败: {} (python={}, script={})", e, self.python_path, self.router_script)
            })?;

        let pid = child.id();
        log::info!("[GPU Manager] router PID: {}", pid);

        // 健康检查轮询（最多等 15 秒）
        for _ in 0..15 {
            thread::sleep(Duration::from_secs(1));
            if self.check_router_health() {
                log::info!("[GPU Manager] router 就绪，LLM 走 GPU 加速");
                if let Ok(mut proc) = self.router_process.lock() {
                    *proc = Some(child);
                }
                self.starting.store(false, Ordering::SeqCst);
                return Ok(());
            }
        }

        // router 没在 15 秒内就绪——可能没有 GPU server，静默 fallback
        log::warn!("[GPU Manager] router 15s 内未就绪，LLM 走 Ollama 直连");
        // 关掉启动失败的进程
        drop(child);
        self.starting.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// 停止 router
    pub fn stop_router(&self) {
        if let Ok(mut proc) = self.router_process.lock() {
            if let Some(mut child) = proc.take() {
                let _ = child.kill();
                let _ = child.wait();
                log::info!("[GPU Manager] router 已停止");
            }
        }
        // 清理端口残留
        let _ = Command::new("lsof")
            .args(&["-ti:11435"])
            .output()
            .map(|o| {
                let pids: Vec<&str> = std::str::from_utf8(&o.stdout)
                    .unwrap_or("")
                    .split_whitespace()
                    .collect();
                for pid in pids {
                    let _ = Command::new("kill").arg(pid).output();
                }
            });
    }

    /// keep-alive 线程：监控 router 健康，崩溃自动拉起
    pub fn start_keepalive(self: std::sync::Arc<Self>) {
        thread::spawn(move || {
            loop {
                thread::sleep(Duration::from_secs(30));
                if !self.is_router_running() {
                    log::warn!("[GPU Manager] router 不在运行，尝试拉起...");
                    if let Err(e) = self.start_router() {
                        log::error!("[GPU Manager] 拉起 router 失败: {}", e);
                    }
                }
            }
        });
    }
}

// 笔尖APP - ASR 服务管理器
// 负责启动/停止 ASR 服务，支持多引擎（FireRed + Sherpa-ONNX）
// FireRed 原生引擎: :8082 (firered_server.py)
// Sherpa-ONNX 统一引擎: :8083 (sherpa_asr_server.py, 支持 SenseVoice/Whisper)

use std::process::{Command, Child};
use std::sync::{Mutex, atomic::{AtomicBool, Ordering}};
use std::time::Duration;
use std::thread;

pub struct AsrManager {
    // FireRed 原生进程
    firered_process: Mutex<Option<Child>>,
    // Sherpa-ONNX 统一进程
    sherpa_process: Mutex<Option<Child>>,
    // 启动中标志（防止并发重复拉起）
    starting: AtomicBool,
    // 主 ASR URL（默认 FireRed）
    primary_asr_url: String,
    // 辅助 ASR URL（Sherpa-ONNX）
    secondary_asr_url: String,
    // 服务器脚本路径
    firered_script: String,
    sherpa_script: String,
    python_path: String,
    firered_pid_file: String,
    sherpa_pid_file: String,
}

impl AsrManager {
    pub fn new() -> Self {
        let project_dir = std::env::var("BIJIAN_PROJECT_DIR")
            .unwrap_or_else(|_| {
                format!("{}/Library/Application Support/PanNote/asr", std::env::var("HOME").unwrap_or_default())
            });

        // PID 文件也迁移到应用私有目录（不再用 /tmp）
        let pid_dir = Self::asr_log_dir();
        std::fs::create_dir_all(&pid_dir).ok();

        Self {
            firered_process: Mutex::new(None),
            sherpa_process: Mutex::new(None),
            starting: AtomicBool::new(false),
            primary_asr_url: "http://127.0.0.1:8082".to_string(),
            secondary_asr_url: "http://127.0.0.1:8083".to_string(),
            firered_script: format!("{}/firered_server.py", project_dir),
            sherpa_script: format!("{}/sherpa_asr_server.py", project_dir),
            python_path: std::env::var("BIJIAN_PYTHON")
                .unwrap_or_else(|_| {
                    "python3".to_string() // 产品版：系统 PATH 中的 python3
                }),
            firered_pid_file: format!("{}/firered_server.pid", pid_dir),
            sherpa_pid_file: format!("{}/sherpa_server.pid", pid_dir),
        }
    }

    /// 检查指定 ASR 服务是否在运行
    fn check_url(&self, url: &str) -> bool {
        match ureq::get(&format!("{}/health", url)).timeout(Duration::from_secs(3)).call() {
            Ok(resp) => resp.status() == 200,
            Err(_) => false,
        }
    }

    /// 检查 ASR 服务是否正在运行（任一可用即可）
    pub fn is_running(&self) -> bool {
        // 检查主引擎
        if self.check_url(&self.primary_asr_url) {
            return true;
        }
        // 检查辅助引擎
        if self.check_url(&self.secondary_asr_url) {
            return true;
        }
        // 检查本地进程
        if let Ok(mut proc) = self.firered_process.lock() {
            if let Some(child) = proc.as_mut() {
                if child.try_wait().ok().flatten().is_none() {
                    return true;
                }
            }
        }
        false
    }

    /// 获取可用的 ASR URL
    pub fn get_asr_url(&self) -> String {
        if self.check_url(&self.primary_asr_url) {
            return self.primary_asr_url.clone();
        }
        if self.check_url(&self.secondary_asr_url) {
            return self.secondary_asr_url.clone();
        }
        self.primary_asr_url.clone()
    }

    /// 获取辅助 ASR URL（用于多引擎对比）
    pub fn get_secondary_asr_url(&self) -> Option<String> {
        if self.check_url(&self.secondary_asr_url) {
            Some(self.secondary_asr_url.clone())
        } else {
            None
        }
    }

    /// 获取应用私有数据目录（用于 PID/日志文件，不再用 /tmp）
/// ~/Library/Application Support/PanNote/asr_logs/
fn asr_log_dir() -> String {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
    format!("{}/Library/Application Support/PanNote/asr_logs", home)
}

/// 启动一个 Python ASR 服务
    fn start_python_server(&self, script: &str, pid_file: &str) -> Result<Child, String> {
        // 清理旧 PID 文件
        if std::path::Path::new(pid_file).exists() {
            if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<u32>() {
                    let _ = Command::new("kill").arg(pid.to_string()).output();
                    thread::sleep(Duration::from_millis(500));
                }
            }
            let _ = std::fs::remove_file(pid_file);
        }

        // 日志写入应用私有目录（不再用 /tmp）
        let log_dir = Self::asr_log_dir();
        std::fs::create_dir_all(&log_dir).ok();
        let log_file = if script.contains("sherpa") {
            format!("{}/sherpa_asr.log", log_dir)
        } else {
            format!("{}/firered_stdout.log", log_dir)
        };
        let err_file = if script.contains("sherpa") {
            format!("{}/sherpa_asr_err.log", log_dir)
        } else {
            format!("{}/firered_stderr.log", log_dir)
        };

        let stdout_file = std::fs::File::create(log_file).map_err(|e| e.to_string())?;
        let stderr_file = std::fs::File::create(err_file).map_err(|e| e.to_string())?;
        
        let child = Command::new(&self.python_path)
            .arg(script)
            .stdout(stdout_file)
            .stderr(stderr_file)
            .spawn()
            .map_err(|e| format!("启动 ASR 服务失败: {} (python={}, script={})", e, self.python_path, script))?;
        
        // 保存 PID
        let pid = child.id();
        if let Err(e) = std::fs::write(pid_file, pid.to_string()) {
            eprintln!("[ASR Manager] 写入 PID 文件失败: {}", e);
        }
        
        Ok(child)
    }

    /// 等待服务就绪
    fn wait_for_url(&self, url: &str, timeout_secs: u64) -> bool {
        for i in 0..timeout_secs {
            thread::sleep(Duration::from_secs(1));
            if self.check_url(url) {
                println!("[ASR Manager] 服务就绪 {} ({}s)", url, i + 1);
                return true;
            }
        }
        false
    }

    /// 确保 ASR 服务正在运行
    pub fn ensure_running(&self) -> Result<(), String> {
        // 先检查是否已有服务在运行
        if self.check_url(&self.primary_asr_url) {
            println!("[ASR Manager] FireRed 服务已在运行");
            // 同时尝试启动 Sherpa 辅助服务
            self.ensure_secondary_running();
            return Ok(());
        }
        if self.check_url(&self.secondary_asr_url) {
            println!("[ASR Manager] Sherpa 服务已在运行（辅助模式）");
            return Ok(());
        }
        
        // 启动 FireRed 主服务
        println!("[ASR Manager] 启动 FireRed 服务...");
        let child = self.start_python_server(&self.firered_script, &self.firered_pid_file)?;
        *self.firered_process.lock().map_err(|e| e.to_string())? = Some(child);
        
        // 等待就绪（FireRed 加载 1.2GB 模型，最多等 60s）
        if self.wait_for_url(&self.primary_asr_url, 60) {
            // FireRed 就绪后，后台启动 Sherpa 辅助服务
            self.ensure_secondary_running();
            return Ok(());
        }
        
        // FireRed 超时，尝试 Sherpa
        println!("[ASR Manager] FireRed 启动超时，尝试 Sherpa 服务...");
        let child2 = self.start_python_server(&self.sherpa_script, &self.sherpa_pid_file)?;
        *self.sherpa_process.lock().map_err(|e| e.to_string())? = Some(child2);
        
        if self.wait_for_url(&self.secondary_asr_url, 30) {
            return Ok(());
        }
        
        Err("ASR 服务启动超时（FireRed 和 Sherpa 均未就绪）".to_string())
    }

    /// launchd agent plist 路径（与 commands.rs::asr_launchd_plist_path 同步）
    fn launchd_plist_path() -> std::path::PathBuf {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        std::path::PathBuf::from(home).join("Library/LaunchAgents/com.bijian.asr.agent.plist")
    }

    /// 后台异步拉起 ASR 服务（不阻塞调用线程）
    /// 用于 app 启动预热 + 录音前保障；就绪状态通过 asr_status / is_running 查询
    /// 注：拉起后进程 detach 常驻（std::process::Child drop 不发 kill），不随本方法返回结束
    ///
    /// 双重拉起防护（v2.3.2）：检测到 launchd agent 已安装时，本进程不 spawn Python，
    /// 由 launchd 独占拉起——根治「AsrManager.spawn + launchd 同时拉起抢 8082/8083 端口」
    /// 导致一方启动失败、launchd KeepAlive 无限重启空转 CPU 的稳定性 bug。
    /// 未装 launchd 时仍保留 spawn fallback，避免冷启动 ASR 不可用。
    pub fn start_in_background(self: std::sync::Arc<Self>) {
        if self.check_url(&self.primary_asr_url) || self.check_url(&self.secondary_asr_url) {
            return; // 已有服务在跑，不重复拉起
        }
        // launchd agent 已安装 → 让 launchd 独占拉起，本进程不 spawn（避免抢端口）
        if Self::launchd_plist_path().exists() {
            eprintln!("[ASR Manager] 检测到 launchd agent 已安装，本进程不 spawn Python，由 launchd 接管拉起");
            return;
        }
        // 防止并发启动：CAS 设标志，已有人在启动则直接返回
        if self.starting.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return; // 已有启动任务在进行中
        }
        std::thread::spawn(move || {
            // FireRed 主引擎：先尝试，最多等 90s（1.2GB 模型冷启动）
            if !self.check_url(&self.primary_asr_url) {
                match self.start_python_server(&self.firered_script, &self.firered_pid_file) {
                    Ok(_child) => {
                        // 丢弃 Child 句柄，进程 detach 常驻（不随 app 退出被杀）
                        for _ in 0..90 {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            if self.check_url(&self.primary_asr_url) {
                                eprintln!("[ASR Manager] 后台启动 FireRed 就绪");
                                break;
                            }
                        }
                    }
                    Err(e) => eprintln!("[ASR Manager] 后台启动 FireRed 失败: {}", e),
                }
            }
            // Sherpa 辅助：FireRed 就绪或失败后，后台拉起 Sherpa（不阻塞）
            if !self.check_url(&self.secondary_asr_url) {
                match self.start_python_server(&self.sherpa_script, &self.sherpa_pid_file) {
                    Ok(_child) => {
                        for _ in 0..60 {
                            std::thread::sleep(std::time::Duration::from_secs(1));
                            if self.check_url(&self.secondary_asr_url) {
                                eprintln!("[ASR Manager] 后台启动 Sherpa 就绪");
                                break;
                            }
                        }
                    }
                    Err(e) => eprintln!("[ASR Manager] 后台启动 Sherpa 失败: {}", e),
                }
            }
            self.starting.store(false, Ordering::SeqCst);
        });
    }

    /// 确保辅助 Sherpa 服务运行（不阻塞，失败不影响主流程）
    fn ensure_secondary_running(&self) {
        if self.check_url(&self.secondary_asr_url) {
            println!("[ASR Manager] Sherpa 辅助服务已在运行");
            return;
        }
        
        println!("[ASR Manager] 后台启动 Sherpa 辅助服务...");
        match self.start_python_server(&self.sherpa_script, &self.sherpa_pid_file) {
            Ok(child) => {
                *self.sherpa_process.lock().unwrap() = Some(child);
                // 在新线程中等就绪
                let url = self.secondary_asr_url.clone();
                thread::spawn(move || {
                    for i in 0..30 {
                        thread::sleep(Duration::from_secs(1));
                        if let Ok(resp) = ureq::get(&format!("{}/health", url)).timeout(Duration::from_secs(3)).call() {
                            if resp.status() == 200 {
                                println!("[ASR Manager] Sherpa 辅助服务就绪 ({}s)", i + 1);
                                return;
                            }
                        }
                    }
                    println!("[ASR Manager] Sherpa 辅助服务启动超时（不影响主引擎）");
                });
            }
            Err(e) => {
                eprintln!("[ASR Manager] Sherpa 辅助服务启动失败: {}（不影响主引擎）", e);
            }
        }
    }

    /// 停止所有 ASR 服务
    pub fn stop(&self) -> Result<(), String> {
        // 停止 FireRed 进程
        if let Ok(mut proc) = self.firered_process.lock() {
            if let Some(mut child) = proc.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        
        // 停止 Sherpa 进程
        if let Ok(mut proc) = self.sherpa_process.lock() {
            if let Some(mut child) = proc.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        
        // 停止 PID 文件中的进程（安全 kill：先验证进程存在 + 命令行含 asr 关键词，避免杀过期 PID 误伤）
        for pid_file in &[&self.firered_pid_file, &self.sherpa_pid_file] {
            if std::path::Path::new(pid_file).exists() {
                if let Ok(pid_str) = std::fs::read_to_string(pid_file) {
                    if let Ok(pid) = pid_str.trim().parse::<u32>() {
                        // 检查进程是否存在 + 命令行是否含 asr/python 关键词
                        let should_kill = match Command::new("ps")
                            .args(["-p", &pid.to_string(), "-o", "command="])
                            .output()
                        {
                            Ok(out) => {
                                let cmdline = String::from_utf8_lossy(&out.stdout).to_lowercase();
                                out.status.success() && !cmdline.is_empty()
                                    && (cmdline.contains("asr") || cmdline.contains("sherpa") || cmdline.contains("firered") || cmdline.contains("python"))
                            }
                            Err(_) => false,
                        };
                        if should_kill {
                            let _ = Command::new("kill").arg(pid.to_string()).output();
                            // 等 500ms 确认退出
                            std::thread::sleep(std::time::Duration::from_millis(500));
                            // 如果还在，SIGKILL 兜底
                            let still_alive = Command::new("ps")
                                .args(["-p", &pid.to_string()])
                                .output()
                                .map(|o| o.status.success())
                                .unwrap_or(false);
                            if still_alive {
                                let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
                            }
                        }
                    }
                }
                let _ = std::fs::remove_file(pid_file);
            }
        }
        
        Ok(())
    }
}

impl Drop for AsrManager {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

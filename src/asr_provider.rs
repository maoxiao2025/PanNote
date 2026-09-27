// PanNote 产品版 v1.0 —— ASR 调用层抽象（trait AsrProvider）
// ============================================================================
// 设计目标（产品化方案 Phase 1）：消除"用户必须装 Python + 虚拟环境 + 7 类模型"的门槛。
//
// 三层能力模型的调用侧统一抽象：
//   EmbeddedAsr（L1 捆绑）—— sherpa-rs 嵌入式 Paraformer + SileroVad + ct-transformer 标点
//                             纯 Rust 进程内调用，零外部服务，安装即用
//   HttpAsr（L2 兼容）      —— 现有 Python HTTP 服务（sherpa_asr_server.py）
//                             开发者/已有部署继续可用，协议不变
//
// 选择逻辑（AsrRegistry::detect）：
//   1. 嵌入式模型文件存在（app_data_dir/models/ 或 ~/.cache/sherpa-models/）→ Embedded 优先
//   2. 否则若 HTTP 服务可达 → HttpAsr
//   3. 两者都不可用 → 不可用状态（App 不崩，前端显示引导）
//
// 线程模型：嵌入式转写是 CPU 密集（实测 473ms/段，4线程），统一走
// tokio::task::spawn_blocking，不阻塞 async runtime。
// ============================================================================

use async_trait::async_trait;
use std::sync::Arc;

/// 段转写结果（与 HTTP 版返回字段对齐，调用侧无感切换）
#[derive(Debug, Clone)]
pub struct SegmentResult {
    pub text: String,
    pub engine: String,          // "embedded_paraformer" / "http_paraformer" / "http_qwen3" ...
    pub fallback: bool,          // 是否发生过引擎回退（供前端可见性提示）
    pub duration: f64,           // 音频时长（秒）——用于段表 end_time 落库
    pub duration_ms: u64,        // 转写耗时（毫秒）
    pub is_silent: bool,         // VAD/电平判定静音段（不调 ASR）
    pub silent_by: String,       // "local_rms"（本地电平）/ "vad"（SileroVad）
}

/// ASR 能力统一接口
#[async_trait]
pub trait AsrProvider: Send + Sync {
    /// 引擎名（落段级表 engine 字段，供统计与 fallback 可见）
    fn name(&self) -> &'static str;
    /// 服务是否可用（模型加载成功/HTTP 可达）
    fn is_available(&self) -> bool;
    /// 转写一段 WAV（16kHz mono PCM，完整 WAV 文件字节，含 RIFF 头）。
    /// 静音段返回 is_silent=true 且不调模型。hotwords 为热词上下文
    /// （嵌入式引擎不支持 per-request 热词，由调用侧后处理纠正兑底）。
    async fn transcribe_wav(&self, wav_bytes: Vec<u8>, hotwords: Option<&str>) -> Result<SegmentResult, String>;
}

// ============================================================================
// 嵌入式实现：sherpa-rs（L1 捆绑层）
// ============================================================================

pub struct EmbeddedAsr {
    recognizer: Arc<std::sync::Mutex<sherpa_rs::paraformer::ParaformerRecognizer>>,
    punct: Arc<std::sync::Mutex<sherpa_rs::punctuate::Punctuation>>,
    /// 预留：VAD 静音段切分（当前架构上层 10s 固定段 + RMS 预检，VAD 留给后续实时切分场景）
    #[allow(dead_code)]
    vad: sherpa_rs::silero_vad::SileroVad,
}

impl EmbeddedAsr {
    /// 加载三件套模型（Paraformer + 标点 + VAD）。模型目录结构：
    ///   {base}/paraformer/model.int8.onnx + tokens.txt
    ///   {base}/punctuation/.../model.int8.onnx
    ///   {base}/vad/silero_vad.onnx
    pub fn load(model_base: &str) -> Result<Self, String> {
        let pf_model = format!("{}/paraformer/model.int8.onnx", model_base);
        let pf_tokens = format!("{}/paraformer/tokens.txt", model_base);
        if !std::path::Path::new(&pf_model).exists() {
            return Err(format!("嵌入式模型缺失: {}", pf_model));
        }
        let recognizer = sherpa_rs::paraformer::ParaformerRecognizer::new(
            sherpa_rs::paraformer::ParaformerConfig {
                model: pf_model,
                tokens: pf_tokens,
                num_threads: Some(4),
                ..Default::default()
            },
        )
        .map_err(|e| format!("Paraformer 加载失败: {:?}", e))?;

        // 标点模型：ct-transformer（int8）。目录名带日期版本，运行时探测
        let punct_model = Self::find_punct_model(model_base);
        let punct = match punct_model {
            Some(p) => sherpa_rs::punctuate::Punctuation::new(
                sherpa_rs::punctuate::PunctuationConfig {
                    model: p,
                    ..Default::default()
                },
            )
            .map_err(|e| format!("标点模型加载失败: {:?}", e))?,
            None => return Err("标点模型缺失（punctuation/ 目录）".to_string()),
        };

        let vad_path = format!("{}/vad/silero_vad.onnx", model_base);
        let vad = sherpa_rs::silero_vad::SileroVad::new(
            sherpa_rs::silero_vad::SileroVadConfig {
                model: vad_path,
                sample_rate: 16000,
                window_size: 512,
                ..Default::default()
            },
            30.0,
        )
        .map_err(|e| format!("VAD 加载失败: {:?}", e))?;

        Ok(Self {
            recognizer: Arc::new(std::sync::Mutex::new(recognizer)),
            punct: Arc::new(std::sync::Mutex::new(punct)),
            vad,
        })
    }

    /// 探测标点模型实际路径（punctuation/sherpa-onnx-punct-*{日期}/model.int8.onnx）
    fn find_punct_model(base: &str) -> Option<String> {
        let dir = format!("{}/punctuation", base);
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let sub = entry.path().join("model.int8.onnx");
            if sub.exists() {
                return Some(sub.to_string_lossy().to_string());
            }
        }
        None
    }
}

#[async_trait]
impl AsrProvider for EmbeddedAsr {
    fn name(&self) -> &'static str {
        "embedded_paraformer"
    }

    fn is_available(&self) -> bool {
        true // 构造成功即可用
    }

    async fn transcribe_wav(&self, wav_bytes: Vec<u8>, _hotwords: Option<&str>) -> Result<SegmentResult, String> {
        // 嵌入式引擎不支持 per-request 热词（Paraformer 热词需构造期加载），
        // hotwords 由调用侧 db_apply_hotwords 后处理兜底
        let samples = decode_wav_16k_mono(&wav_bytes)?;
        let t0 = std::time::Instant::now();
        let duration = samples.len() as f64 / 16000.0;

        // 静音预检（RMS 电平，与现有本地电平检测一致——全零/近零段不调模型）
        let rms = samples.iter().map(|s| s * s).sum::<f32>() / samples.len().max(1) as f32;
        if rms.sqrt() < 0.001 {
            return Ok(SegmentResult {
                text: String::new(),
                engine: self.name().to_string(),
                fallback: false,
                duration,
                duration_ms: t0.elapsed().as_millis() as u64,
                is_silent: true,
                silent_by: "local_rms".to_string(),
            });
        }

        // 转写（blocking）。Arc<Mutex> 内部可变性：与 unsafe 指针 cast（UB，被
        // invalid_reference_casting lint 拒绝）等效的合法写法；Mutex 保证串行调用
        let rec_arc = self.recognizer.clone();
        let text = tokio::task::spawn_blocking(move || {
            let mut rec = rec_arc.lock().map_err(|_| "ASR 引擎锁中毒".to_string())?;
            Ok::<_, String>(rec.transcribe(16000, &samples).text)
        })
        .await
        .map_err(|e| format!("blocking join 失败: {}", e))??;

        // 标点恢复（36ms/段，也放 blocking）。任何失败兜底原文（无标点但不丢字）
        let punctuated = if !text.trim().is_empty() {
            let value = text.clone();
            let punct_arc = self.punct.clone();
            tokio::task::spawn_blocking(move || {
                punct_arc.lock().ok().map(|mut p| p.add_punctuation(&value))
            })
            .await
            .ok()
            .flatten()
            .unwrap_or(text)
        } else {
            text
        };

        Ok(SegmentResult {
            text: punctuated,
            engine: self.name().to_string(),
            fallback: false,
            duration,
            duration_ms: t0.elapsed().as_millis() as u64,
            is_silent: false,
            silent_by: String::new(),
        })
    }
}

// ============================================================================
// HTTP 实现：现有 Python 服务（L2 兼容层，协议与开发版完全一致）
// ============================================================================

pub struct HttpAsr {
    pub url: String, // 如 http://127.0.0.1:8083
    pub client: reqwest::Client,
}

#[async_trait]
impl AsrProvider for HttpAsr {
    fn name(&self) -> &'static str {
        "http_paraformer"
    }

    fn is_available(&self) -> bool {
        // 探测 /health（同步 ureq，启动期调用可接受）
        ureq::get(&format!("{}/health", self.url))
            .timeout(std::time::Duration::from_secs(3))
            .call()
            .map(|_| true)
            .unwrap_or(false)
    }

    async fn transcribe_wav(&self, wav_bytes: Vec<u8>, hotwords: Option<&str>) -> Result<SegmentResult, String> {
        // 协议对齐现有 /qwen3_transcribe：multipart WAV 上传（字节原样透传，
        // Python 端兼容任意格式 wav/mp3/...，嵌入式做不到的格式由这层兜底）
        let t0 = std::time::Instant::now();
        let form = reqwest::multipart::Form::new().part(
            "file",
            reqwest::multipart::Part::bytes(wav_bytes).file_name("seg.wav"),
        );
        let url = match hotwords {
            Some(h) if !h.is_empty() => format!(
                "{}/qwen3_transcribe?context={}",
                self.url,
                urlencoding::encode(h)
            ),
            _ => format!("{}/qwen3_transcribe", self.url),
        };
        let resp: serde_json::Value = self
            .client
            .post(&url)
            .timeout(std::time::Duration::from_secs(120))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("ASR 请求失败: {}", e))?
            .json()
            .await
            .unwrap_or_default();

        let text = resp.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string();
        let engine = resp.get("engine").and_then(|e| e.as_str()).unwrap_or("http").to_string();
        let fallback = resp.get("fallback").and_then(|f| f.as_bool()).unwrap_or(false);
        let duration = resp.get("duration").and_then(|d| d.as_f64()).unwrap_or(0.0);
        let is_silent = text.is_empty() || text == "<sil>" || resp.get("silent").and_then(|s| s.as_bool()).unwrap_or(false);

        Ok(SegmentResult {
            text,
            engine: if fallback { format!("fallback_{}", engine) } else { engine },
            fallback,
            duration,
            duration_ms: t0.elapsed().as_millis() as u64,
            is_silent,
            silent_by: if is_silent { "asr_vad".to_string() } else { String::new() },
        })
    }
}

// ============================================================================
// 注册表：启动探测 + 统一出口
// ============================================================================

/// 全局注册表槽（None = 启动探测中；Some = 探测完成，含"不可用"降级态）。
/// 全局静态的理由：三处转写调用点（实时循环/补跑/上传）中前两处在 spawn 任务里，
/// 无 AppState 可用；项目已有 recording_lock() 全局先例，避免签名链大面积改造。
static ASR_REGISTRY: once_cell::sync::Lazy<std::sync::RwLock<Option<Arc<AsrRegistry>>>> =
    once_cell::sync::Lazy::new(|| std::sync::RwLock::new(None));

/// 附加模型目录候选（打包进 App 的 Resources/models/，由 lib.rs 启动时注册）。
/// 产品版 v1.0：模型打进 dmg（.app/Contents/Resources/models/），此路径优先级高于开发缓存。
static EXTRA_MODEL_BASES: once_cell::sync::Lazy<std::sync::Mutex<Vec<String>>> =
    once_cell::sync::Lazy::new(|| std::sync::Mutex::new(Vec::new()));

/// 注册附加模型目录（App 资源目录下的 models/）
pub fn register_model_base_hint(base: String) {
    if let Ok(mut v) = EXTRA_MODEL_BASES.lock() {
        v.push(base);
    }
}

/// 探测完成后的注册表快照（探测中返回 None）
pub fn registry_snapshot() -> Option<Arc<AsrRegistry>> {
    ASR_REGISTRY.read().ok().and_then(|g| g.clone())
}

/// 注册探测结果（lib.rs 启动线程调用，一生一次）
pub fn registry_install(reg: AsrRegistry) {
    if let Ok(mut slot) = ASR_REGISTRY.write() {
        *slot = Some(Arc::new(reg));
    }
}

/// 转写一段 WAV（16kHz mono）——三处转写调用点的统一出口。
/// 探测未完成时轮询等待（模型加载 5.3s 量级，启动后前几段可能撞上），最长 wait_secs。
pub async fn transcribe_wav_global(
    wav_bytes: Vec<u8>,
    hotwords: Option<&str>,
    wait_secs: u64,
) -> Result<SegmentResult, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        match registry_snapshot() {
            Some(reg) => return reg.transcribe(wav_bytes, hotwords).await,
            None => {
                if std::time::Instant::now() >= deadline {
                    return Err("ASR 引擎仍在启动中（模型加载），请稍后重试".to_string());
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

pub struct AsrRegistry {
    provider: Option<Arc<dyn AsrProvider>>,
    /// 探测日志（供健康面板显示当前引擎与降级原因）
    pub detect_log: String,
}

impl AsrRegistry {
    /// 启动探测：嵌入式优先，HTTP 兜底
    /// model_base 候选：App 资源目录 models（打包）→ App 数据目录 models → ~/.cache/sherpa-models（开发机兼容）
    pub fn detect() -> Self {
        let mut log = String::new();
        let home = std::env::var("HOME").unwrap_or_default();

        // 候选 1：App 资源目录（产品版打包位置，register_model_base_hint 注册）
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(extra) = EXTRA_MODEL_BASES.lock() {
            candidates.extend(extra.iter().cloned());
        }
        // 候选 2：App 数据目录（用户自定义放置）
        candidates.push(format!("{}/Library/Application Support/com.bijian.app.pro/models", home));
        // 候选 3：开发机缓存（~/.cache/sherpa-models）
        candidates.push(format!("{}/.cache/sherpa-models", home));

        for base in &candidates {
            match EmbeddedAsr::load(base) {
                Ok(embedded) => {
                    log.push_str(&format!("ASR 引擎：嵌入式 Paraformer（模型目录 {}）", base));
                    return Self {
                        provider: Some(Arc::new(embedded)),
                        detect_log: log,
                    };
                }
                Err(e) => log.push_str(&format!("嵌入式探测失败（{}）：{}\n", base, e)),
            }
        }

        // 候选 2：HTTP 服务（开发版部署）
        let url = std::env::var("BIJIAN_SHERPA_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8083".to_string());
        let http = HttpAsr {
            url: url.clone(),
            client: reqwest::Client::new(),
        };
        if http.is_available() {
            log.push_str(&format!("ASR 引擎：HTTP 服务（{}）", url));
            return Self {
                provider: Some(Arc::new(http)),
                detect_log: log,
            };
        }
        log.push_str(&format!("HTTP 服务不可达（{}）\n", url));

        // 都不可用：App 继续跑（录音/笔记可用），转写引导由前端显示
        log.push_str("ASR 不可用：无嵌入式模型且 HTTP 服务离线。录音仍可用，转写功能需安装模型。\n");
        Self { provider: None, detect_log: log }
    }

    pub fn available(&self) -> bool {
        self.provider.is_some()
    }

    /// 当前引擎名（None = 不可用）。供启动逻辑判断是否还需要拉起 Python 服务
    pub fn engine_name(&self) -> Option<&'static str> {
        self.provider.as_ref().map(|p| p.name())
    }

    pub async fn transcribe(&self, wav_bytes: Vec<u8>, hotwords: Option<&str>) -> Result<SegmentResult, String> {
        match &self.provider {
            Some(p) => p.transcribe_wav(wav_bytes, hotwords).await,
            None => Err("ASR 不可用（无模型且服务离线）".to_string()),
        }
    }
}

// ============================================================================
// 工具函数
// ============================================================================

/// WAV 字节 → 16kHz mono f32 采样（嵌入式引擎输入格式）。
/// 仅支持 16kHz 单声道 16bit PCM（录音段文件的标准格式）。
/// 其他采样率/声道/位深返回明确错误（L1 模式限制，HTTP 模式无此限制）。
pub fn decode_wav_16k_mono(wav_bytes: &[u8]) -> Result<Vec<f32>, String> {
    let cursor = std::io::Cursor::new(wav_bytes.to_vec());
    let mut reader = hound::WavReader::new(cursor).map_err(|e| {
        format!("音频格式不支持：嵌入式引擎仅支持 WAV（当前文件无法解析，{:?}）。请上传 WAV 格式，或安装完整版使用多格式支持", e)
    })?;
    let spec = reader.spec();
    if spec.sample_rate != 16000 {
        return Err(format!(
            "嵌入式引擎仅支持 16kHz 音频（当前 {}Hz）。请转换格式后重试",
            spec.sample_rate
        ));
    }
    if spec.channels != 1 {
        return Err(format!(
            "嵌入式引擎仅支持单声道音频（当前 {} 声道）",
            spec.channels
        ));
    }
    if spec.sample_format != hound::SampleFormat::Int || spec.bits_per_sample != 16 {
        return Err("嵌入式引擎仅支持 16bit PCM WAV".to_string());
    }
    let samples: Vec<f32> = reader
        .samples::<i16>()
        .filter_map(|s| s.ok())
        .map(|s| s as f32 / 32768.0)
        .collect();
    Ok(samples)
}


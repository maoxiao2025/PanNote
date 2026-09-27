// 产品化 v1.0 嵌入式 ASR 集成测试（改版必跑）
// 验证 L1 捆绑层在产品版 lib 内真实可用：模型加载 → WAV 解码 → Paraformer 转写 → 标点恢复。
// 前置：~/.cache/sherpa-models/ 三件套模型（373MB）+ /tmp/probe_test.wav（9.8s 中文财务内容）。
// 标记 ignored：无模型的干净机器（CI/新装用户首次启动）上不跑，本机验证用 --ignored 跑。
// 设计对照：纯函数 decode 单测不需要模型，始终跑。

use bijian::asr_provider::{decode_wav_16k_mono, AsrProvider, EmbeddedAsr};

/// 生成 N 秒全零静音 WAV（16kHz mono 16bit PCM，与录音段文件同格式）
fn make_silent_wav_16k(seconds: f64) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        for _ in 0..(16000.0 * seconds) as u32 {
            writer.write_sample(0i16).unwrap();
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

/// 生成 N 秒非全零"有声"WAV（方波，验证解码与采样值转换，不跑模型）
fn make_tone_wav_16k(seconds: f64) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 16000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = hound::WavWriter::new(&mut cursor, spec).unwrap();
        let total = (16000.0 * seconds) as u32;
        for i in 0..total {
            let phase = (i % 100) < 50;
            writer.write_sample(if phase { 12000i16 } else { -12000 }).unwrap();
        }
        writer.finalize().unwrap();
    }
    cursor.into_inner()
}

// ============================================================================
// 纯函数层（无模型依赖，始终跑）
// ============================================================================

#[test]
fn decode_accepts_16k_mono_pcm() {
    let wav = make_tone_wav_16k(0.5);
    let samples = decode_wav_16k_mono(&wav).expect("标准 16k mono WAV 解码失败");
    assert_eq!(samples.len(), 8000, "0.5s @16kHz 应有 8000 采样");
    assert!((samples[0] - 12000.0 / 32768.0).abs() < 0.01, "采样值转换错误");
}

#[test]
fn decode_rejects_non_wav_bytes_with_guidance() {
    let bad = decode_wav_16k_mono(b"this is an mp3 or garbage".to_vec().as_slice());
    assert!(bad.is_err(), "非 WAV 字节未报错");
    let msg = bad.unwrap_err();
    assert!(msg.contains("WAV"), "错误信息应引导 WAV 格式：{}", msg);
}

/// 降级路径（产品化关键场景：新用户无模型、无 Python 服务，App 不崩）：
/// 模型目录不存在 → 明确报错；HTTP 服务不可达 → is_available=false。
/// 完整的干净启动实测在 dmg 打包环节（RELEASE_CHECKLIST"无 Python 环境实测"项）。
#[test]
fn degrade_path_without_models_or_service() {
    // 1. 模型目录不存在 → EmbeddedAsr::load 明确报错（含"缺失"引导）
    let e = match EmbeddedAsr::load("/tmp/pannote_nonexistent_models_dir") {
        Err(e) => e,
        Ok(_) => panic!("不存在的模型目录不应加载成功"),
    };
    assert!(e.contains("缺失"), "模型缺失错误信息应含引导：{}", e);

    // 2. HTTP 服务不可达（本地保留端口，连接拒绝即时返回）→ 不 panic、判不可用
    let http = bijian::asr_provider::HttpAsr {
        url: "http://127.0.0.1:59999".to_string(),
        client: reqwest::Client::new(),
    };
    assert!(!http.is_available(), "不可达端口应判定不可用");
    assert_eq!(http.name(), "http_paraformer");
}

// ============================================================================
// 嵌入式引擎全链路（需本机模型，--ignored 跑）
// ============================================================================

#[tokio::test]
#[ignore = "需本机模型缓存（~/.cache/sherpa-models，373MB）与测试音频 /tmp/probe_test.wav"]
async fn embedded_asr_full_chain() {
    let home = std::env::var("HOME").unwrap();
    let model_base = format!("{}/.cache/sherpa-models", home);

    // ---- 1. 三件套模型加载（基线 5.3s，上限 30s）----
    let t0 = std::time::Instant::now();
    let asr = EmbeddedAsr::load(&model_base).expect("嵌入式模型加载失败");
    let load_ms = t0.elapsed().as_millis();
    println!("[1] 模型加载: {}ms", load_ms);
    assert!(load_ms < 30_000, "模型加载超过 30s: {}ms", load_ms);

    // ---- 2. 真实音频转写（9.8s 中文财务内容）----
    let wav = std::fs::read("/tmp/probe_test.wav").expect("测试音频缺失 /tmp/probe_test.wav");
    let t1 = std::time::Instant::now();
    let r = asr.transcribe_wav(wav, None).await.expect("转写失败");
    let t_ms = t1.elapsed().as_millis();
    println!("[2] 转写: {}ms → {}", t_ms, r.text);
    println!("    engine={} duration={}s is_silent={}", r.engine, r.duration, r.is_silent);

    assert!(!r.is_silent, "真实语音被误判静音");
    assert!(!r.text.is_empty(), "转写结果为空");
    assert_eq!(r.engine, "embedded_paraformer");
    assert!(
        (r.duration - 9.8).abs() < 0.5,
        "duration 偏差过大: {}（期望 ~9.8s）",
        r.duration
    );
    // 内容校验：至少 5 个中文字符（准确度对齐验证工程基线）
    let cjk_count = r.text.chars().filter(|c| c.len_utf8() > 1).count();
    assert!(cjk_count >= 5, "无有效中文内容（{} 个非 ASCII 字符）", cjk_count);
    // 性能基线：转写耗时 < 音频时长（RTF < 1，实时无积压；实测基线 473ms/9.8s 段）
    assert!(
        (t_ms as f64) < r.duration * 1000.0,
        "转写耗时 {}ms 超过音频时长 {}ms，不满足实时",
        t_ms,
        r.duration * 1000.0
    );
    // 标点校验：ct-transformer 应恢复中文标点
    assert!(
        r.text.contains('。') || r.text.contains('，') || r.text.contains('、'),
        "标点恢复缺失: {}",
        r.text
    );

    // ---- 3. 全零静音段：RMS 预检拦截，不调模型（应 <100ms）----
    let silent_wav = make_silent_wav_16k(1.0);
    let t2 = std::time::Instant::now();
    let r2 = asr.transcribe_wav(silent_wav, None).await.expect("静音段调用失败");
    let silent_ms = t2.elapsed().as_millis();
    println!("[3] 静音段: is_silent={} silent_by={} {}ms", r2.is_silent, r2.silent_by, silent_ms);
    assert!(r2.is_silent, "全零段未被 RMS 预检拦截");
    assert_eq!(r2.silent_by, "local_rms");
    assert!(r2.text.is_empty(), "静音段不应有文本");
    assert!(silent_ms < 500, "静音预检耗时异常: {}ms", silent_ms);

    // ---- 4. 非音频字节：明确格式引导（产品语义：L1 模式限制）----
    let bad = asr.transcribe_wav(b"not a wav file".to_vec(), None).await;
    assert!(bad.is_err(), "非 WAV 字节未报错");
    let msg = bad.unwrap_err();
    assert!(msg.contains("WAV"), "错误信息应引导 WAV 格式：{}", msg);
    println!("[4] 非 WAV 错误信息: {}", msg);
}

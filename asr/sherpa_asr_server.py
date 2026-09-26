#!/usr/bin/env python3
"""
Sherpa-ONNX ASR HTTP Server
统一多引擎 ASR 服务，支持 FireRed(AED) + SenseVoice + Whisper
端口: 8083 (与 firered_server.py 的 8082 区分)

端点:
  GET  /health        - 健康检查
  POST /transcribe    - 转写音频（支持 engine 参数选择引擎）
  GET  /engines       - 列出可用引擎
"""

import os
import sys
import json
import tempfile
import wave
import time
import asyncio
import concurrent.futures
import threading
import subprocess
from aiohttp import web
import numpy as np

# === 资源协调（P7: 避免与 Ollama 4B 抢资源）===
# Qwen3-ASR 的 LlamaContext 非线程安全：并发转写会偶发 GGML_ASSERT 崩溃。
# 双锁设计（v6）：
#   - ASR_INFER_LOCK：精转引擎锁（1.7B，滚动/全量精转用），串行化精转推理
#   - ASR_QUICK_LOCK：快速引擎锁（0.6B，准实时段转写用），独立于精转锁
# 两把锁互不阻塞：精转跑时不阻塞实时转写，实时转写也不等精转（空闲窗口机制）。
ASR_INFER_LOCK = threading.Lock()   # 精转锁（1.7B）

# 2026-09-03: 快速引擎单实例+单锁是积压根因（0.6B 单段 ~10s 跟不上 10s/段的录音节奏）
# 改为双实例负载均衡：每个实例独立持锁，轮询分配，并发度×2（llama context 非线程安全，不可共享）
ASR_QUICK_ENGINES: list = []          # [(engine, threading.Lock()), ...]，启动时填充
ASR_QUICK_ROUND_ROBIN = 0             # 轮询游标
ASR_QUICK_LOCK = threading.Lock()     # 仅保护轮询计数（非推理锁）

# 负载感知调度：转写前检查系统 loadavg，高负载时先等待 Ollama 让出资源。
# 阈值基于 12 核机器：正常 ≤6，繁忙 >8，满载 >12
# 2026-09-03: 双实例后放宽（快速引擎排队 60s 会饿死实时转写，是积压主因之一）
LOAD_HIGH = 10.0      # loadavg 超过此值视为高负载（原 8.0）
LOAD_MAX_WAIT = 30.0  # 高负载时最长等待（秒）（原 60.0）

def get_loadavg():
    """获取系统 1 分钟负载（macOS sysctl）"""
    try:
        out = subprocess.run(
            ["sysctl", "-n", "vm.loadavg"],
            capture_output=True, text=True, timeout=2,
        ).stdout
        parts = out.strip().split()
        return float(parts[1])
    except Exception:
        return 0.0

# 别名：统一用 get_load()
get_load = get_loadavg


def wait_for_low_load():
    """负载感知排队：负载过高时等待，最多 LOAD_MAX_WAIT 秒。
    返回 (等待秒数, 是否在等待窗口内降下来)。
    """
    waited = 0.0
    while waited < LOAD_MAX_WAIT:
        load = get_load()
        if load < LOAD_HIGH:
            return waited, True
        time.sleep(2.0)
        waited += 2.0
    return waited, False


def ollama_unload_all():
    """卸载 Ollama 中常驻的模型（释放 CPU 给 ASR 转写）"""
    try:
        subprocess.run(
            ["curl", "-s", "-X", "POST", "http://127.0.0.1:11434/api/generate",
             "-d", '{"model": "qwen3-4b-32k", "keep_alive": 0}'],
            capture_output=True, timeout=3,
        )
        return True
    except Exception:
        return False


def run_asr_locked(fn, *args, lock=None, **kwargs):
    """在指定锁内执行 ASR 推理，避免并发崩 llama context。
    执行前若系统负载过高先等待。
    lock: 默认用精转锁 ASR_INFER_LOCK；快速引擎传 ASR_QUICK_LOCK。"""
    waited, ok = wait_for_low_load()
    if not ok:
        print(f"[ASR] 负载持续过高，仍继续（负载 {get_load():.1f}）", flush=True)
    with (lock or ASR_INFER_LOCK):
        return fn(*args, **kwargs)


def denoise_wav(wav_path, strength="medium"):
    """对 WAV 文件做降噪，返回降噪后文件路径（临时文件，调用方负责清理）。

    使用 ffmpeg afftdn filter（FFT 降噪，内置无需额外安装）：
      - light:   afftdn=nf=-25 (轻度)
      - medium:  afftdn=nf=-35 (中度，默认)
      - strong:  afftdn=nf=-45 (重度)
    降噪只影响喂给 ASR 的音频，不修改原始录音。
    """
    nr = {"light": "-25", "medium": "-35", "strong": "-45"}.get(strength, "-35")
    tmp = tempfile.NamedTemporaryFile(suffix="_denoised.wav", delete=False)
    tmp.close()
    try:
        r = subprocess.run(
            ["ffmpeg", "-y", "-i", wav_path,
             "-af", f"afftdn=nf={nr}",
             "-ar", "16000", "-ac", "1", "-acodec", "pcm_s16le",
             tmp.name],
            capture_output=True, timeout=300,
        )
        if r.returncode != 0 or os.path.getsize(tmp.name) < 1000:
            print(f"[Denoise] ffmpeg 失败: {r.stderr.decode(errors='replace')[-200:]}", flush=True)
            os.unlink(tmp.name)
            return wav_path  # 降噪失败用原始音频
        return tmp.name
    except Exception as e:
        print(f"[Denoise] 异常: {e}", flush=True)
        if os.path.exists(tmp.name):
            os.unlink(tmp.name)
        return wav_path

# === 引擎配置 ===

FIRERED_DIR = os.path.expanduser(
    "~/.cache/modelscope/models/manyeyes--fireredasr2-aed-large-zh-en-int8-onnx-offline-20260212/snapshots/master"
)
SENSEVOICE_DIR = os.path.expanduser("~/.cache/sherpa-models/sensevoice")
WHISPER_CLI = "/usr/local/bin/whisper-cli"
WHISPER_MODEL_DIR = os.path.expanduser("~/.cache/whisper-models")

# 说话人分离模型
SEG_MODEL = os.path.expanduser(
    "~/.cache/sherpa-models/speaker-diarization/sherpa-onnx-pyannote-segmentation-3-0/model.int8.onnx"
)
EMBED_MODEL = os.path.expanduser(
    "~/.cache/sherpa-models/speaker-diarization/3dspeaker_speech_eres2net_base_sv_zh-cn_3dspeaker_16k.onnx"
)

# P5: VAD 模型
VAD_MODEL = os.path.expanduser("~/.cache/sherpa-models/vad/silero_vad.onnx")

# P5: 标点恢复模型 (ct-transformer)
PUNCT_MODEL = os.path.expanduser(
    "~/.cache/sherpa-models/punctuation/sherpa-onnx-punct-ct-transformer-zh-en-vocab272727-2024-04-12-int8/model.int8.onnx"
)

# P5: 声纹库持久化目录
VOICEPRINT_DIR = os.path.expanduser("~/.cache/sherpa-models/voiceprints")

PYTHON_PATH = os.environ.get(
    "BIJIAN_PYTHON",
    "python3",  # 产品版：不含开发者私有路径，用系统 PATH 中的 python3
)

# === 全局引擎 ===

engines = {}  # name -> recognizer

# v2.5.0（9-25）实时段引擎切换：BIJIAN_REALTIME_ENGINE=paraformer 时启用。
# Paraformer RTF 0.03（实测快 28 倍），根治“10s 段 58s 转、停止时积压 7 段”的损失窗口。
# 不设环境变量 = 现状（0.6B 双实例池），一行回滚。
# v2.5.1（9-25）：清理合并时残留的重复声明块（168-178 原来声明了两次，改一处漏一处隐患）
REALTIME_ENGINE = os.environ.get("BIJIAN_REALTIME_ENGINE", "").lower()
PARAFORMER_RT = None
diarizer = None  # OfflineSpeakerDiarization 实例
executor = concurrent.futures.ThreadPoolExecutor(max_workers=4)

# P5: VAD 配置（每次请求创建新实例）
vad_config = None

# P5: 标点恢复模型
punct_model = None

# P5: 声纹提取器和管理器
embedding_extractor = None
embedding_manager = None

# Qwen3-ASR 引擎全局单例
qwen3_asr_engine = None
# P8: 0.6B 快速引擎（准实时段转写用，快 RTF~0.27）
qwen3_asr_engine_quick = None


def init_sensevoice():
    """初始化 SenseVoice 引擎"""
    model_path = os.path.join(SENSEVOICE_DIR, "model.int8.onnx")
    tokens_path = os.path.join(SENSEVOICE_DIR, "tokens.txt")

    if not os.path.exists(model_path) or not os.path.exists(tokens_path):
        print(f"[SenseVoice] 模型文件不完整: {model_path}")
        return None

    # 检查模型大小（至少 200MB）
    if os.path.getsize(model_path) < 200 * 1024 * 1024:
        print(f"[SenseVoice] 模型文件不完整: {os.path.getsize(model_path) / 1024 / 1024:.1f} MB")
        return None

    try:
        import sherpa_onnx

        recognizer = sherpa_onnx.OfflineRecognizer.from_sense_voice(
            model=model_path,
            tokens=tokens_path,
            use_itn=True,
            num_threads=2,
            debug=False,
            provider="cpu",
        )
        print(f"[SenseVoice] 加载成功")
        return recognizer
    except Exception as e:
        print(f"[SenseVoice] 加载失败: {e}")
        return None


def init_firered_aed():
    """初始化 FireRed AED 引擎（通过 sherpa-onnx）"""
    encoder = os.path.join(FIRERED_DIR, "encoder.int8.onnx")
    decoder = os.path.join(FIRERED_DIR, "decoder.int8.onnx")
    tokens = os.path.join(FIRERED_DIR, "tokens.txt")

    if not all(os.path.exists(p) for p in [encoder, decoder, tokens]):
        print(f"[FireRed AED] 模型文件不完整")
        return None

    try:
        import sherpa_onnx

        recognizer = sherpa_onnx.OfflineRecognizer.from_fire_red_asr(
            encoder=encoder,
            decoder=decoder,
            tokens=tokens,
            num_threads=2,
            debug=False,
            provider="cpu",
        )
        print(f"[FireRed AED] 加载成功")
        return recognizer
    except Exception as e:
        print(f"[FireRed AED] 加载失败: {e}")
        return None


def init_firered_native():
    """初始化 FireRed 原生引擎（通过 firered_asr.py）"""
    try:
        firered_lib_dir = os.environ.get(
            "FIRERED_LIB_DIR",
            os.path.expanduser("~/Library/Application Support/PanNote/models/firered_lib"),
        )
        sys.path.insert(0, firered_lib_dir)
        from firered_asr import FireRedASR

        engine = FireRedASR()
        print(f"[FireRed Native] 加载成功")
        return engine
    except Exception as e:
        print(f"[FireRed Native] 加载失败: {e}")
        return None


def init_vad():
    """初始化 Silero VAD 配置"""
    if not os.path.exists(VAD_MODEL):
        print(f"[VAD] 模型文件不存在: {VAD_MODEL}")
        return False
    try:
        import sherpa_onnx
        global vad_config
        vad_config = sherpa_onnx.VadModelConfig()
        vad_config.silero_vad.model = VAD_MODEL
        vad_config.silero_vad.threshold = 0.5
        vad_config.sample_rate = 16000
        # 注意：单位是秒不是毫秒
        vad_config.silero_vad.min_silence_duration = 0.5
        vad_config.silero_vad.min_speech_duration = 0.25
        vad_config.silero_vad.max_speech_duration = 20.0
        # 测试加载
        vad = sherpa_onnx.VoiceActivityDetector(vad_config, buffer_size_in_seconds=60)
        del vad
        print(f"[VAD] Silero VAD 配置成功")
        return True
    except Exception as e:
        print(f"[VAD] 加载失败: {e}")
        return False


def init_punctuation():
    """初始化 ct-transformer 标点恢复模型"""
    if not os.path.exists(PUNCT_MODEL):
        print(f"[Punctuation] 模型文件不存在: {PUNCT_MODEL}")
        return None
    try:
        import sherpa_onnx
        config = sherpa_onnx.OfflinePunctuationConfig(
            model=sherpa_onnx.OfflinePunctuationModelConfig(
                ct_transformer=PUNCT_MODEL,
            )
        )
        punct = sherpa_onnx.OfflinePunctuation(config)
        # 测试
        test = punct.add_punctuation("你好世界这是一段测试")
        print(f"[Punctuation] ct-transformer 加载成功, 测试: '{test}'")
        return punct
    except Exception as e:
        print(f"[Punctuation] 加载失败: {e}")
        return None


def init_embedding_extractor():
    """初始化声纹提取器和管理器"""
    if not os.path.exists(EMBED_MODEL):
        print(f"[Voiceprint] 嵌入模型不存在: {EMBED_MODEL}")
        return None, None
    try:
        import sherpa_onnx
        extractor = sherpa_onnx.SpeakerEmbeddingExtractor(
            sherpa_onnx.SpeakerEmbeddingExtractorConfig(
                model=EMBED_MODEL,
                num_threads=1,
                debug=False,
            )
        )
        manager = sherpa_onnx.SpeakerEmbeddingManager(extractor.dim)

        # 加载已保存的声纹
        os.makedirs(VOICEPRINT_DIR, exist_ok=True)
        loaded = 0
        for f in os.listdir(VOICEPRINT_DIR):
            if f.endswith('.npy'):
                name = f[:-4]
                embedding = np.load(os.path.join(VOICEPRINT_DIR, f))
                manager.add(name, embedding)
                loaded += 1
        print(f"[Voiceprint] 提取器加载成功, 已加载 {loaded} 个声纹")
        return extractor, manager
    except Exception as e:
        print(f"[Voiceprint] 加载失败: {e}")
        return None, None


def transcribe_sensevoice(recognizer, wav_path):
    """用 SenseVoice 转写"""
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    stream = recognizer.create_stream()
    stream.accept_waveform(sample_rate, audio)
    recognizer.decode_stream(stream)

    return {
        "text": stream.result.text,
        "duration": len(audio) / sample_rate,
        "engine": "sensevoice",
    }


def transcribe_firered_aed(recognizer, wav_path):
    """用 FireRed AED (sherpa-onnx) 转写"""
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    stream = recognizer.create_stream()
    stream.accept_waveform(sample_rate, audio)
    recognizer.decode_stream(stream)

    return {
        "text": stream.result.text,
        "duration": len(audio) / sample_rate,
        "engine": "firered_aed",
    }


def transcribe_firered_native(engine, wav_path):
    """用 FireRed 原生引擎转写"""
    result = engine.transcribe(wav_path)
    return {
        "text": result["text"],
        "duration": result["duration"],
        "engine": "firered_native",
    }


def transcribe_whisper_cpp(wav_path):
    """用 whisper.cpp 转写"""
    import subprocess

    # 查找模型
    model_candidates = [
        os.path.join(WHISPER_MODEL_DIR, "ggml-tiny.bin"),
        os.path.join(WHISPER_MODEL_DIR, "ggml-base.bin"),
    ]
    model_path = next((p for p in model_candidates if os.path.exists(p) and os.path.getsize(p) > 1000000), None)

    if not model_path or not os.path.exists(WHISPER_CLI):
        return None

    try:
        output = subprocess.run(
            [WHISPER_CLI, "-m", model_path, "-f", wav_path, "-l", "zh", "-nt"],
            capture_output=True,
            text=True,
            timeout=120,
        )
        # whisper-cli 输出格式：每行带时间戳，-nt 去时间戳
        text = output.stdout.strip()
        # 去掉 whisper 的 header 输出
        lines = [l for l in text.split("\n") if l and not l.startswith("whisper_")]
        text = " ".join(lines).strip()

        return {
            "text": text,
            "duration": 0,
            "engine": "whisper_cpp",
        }
    except Exception as e:
        print(f"[whisper.cpp] 错误: {e}")
        return None


# === P5 辅助函数 ===


def run_vad_transcribe_impl(wav_path):
    """VAD 智能分段 + 逐段转写"""
    import sherpa_onnx

    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    # 每次请求创建新 VAD 实例
    vad = sherpa_onnx.VoiceActivityDetector(vad_config, buffer_size_in_seconds=60)

    # 分块喂给 VAD（Silero VAD 需要 1024 样本 = 64ms@16kHz）
    chunk_size = 1024
    for i in range(0, len(audio), chunk_size):
        chunk = audio[i:i + chunk_size]
        if len(chunk) < chunk_size:
            chunk = np.pad(chunk, (0, chunk_size - len(chunk)))
        vad.accept_waveform(chunk)

    # Flush 获取最后的语音段
    try:
        vad.flush()
    except Exception:
        pass

    # 选择转写引擎
    use_firered_native = False
    recognizer = None
    engine_name = "none"
    if "firered_native" in engines:
        recognizer = engines["firered_native"]
        engine_name = "firered_native"
        use_firered_native = True
    elif "firered_aed" in engines:
        recognizer = engines["firered_aed"]
        engine_name = "firered_aed"
    elif "sensevoice" in engines:
        recognizer = engines["sensevoice"]
        engine_name = "sensevoice"
    else:
        return [{"text": "", "start": 0, "end": 0, "error": "no engine available"}]

    results = []
    while not vad.empty():
        segment = vad.front
        seg_samples = np.array(segment.samples, dtype=np.float32)
        seg_duration = len(seg_samples) / 16000.0

        if seg_duration >= 0.3:
            if use_firered_native:
                # FireRedASR 用 transcribe 方法，需要写临时 WAV 文件
                import tempfile
                tmp_wav = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
                tmp_wav.close()
                with wave.open(tmp_wav.name, "w") as wf:
                    wf.setnchannels(1)
                    wf.setsampwidth(2)
                    wf.setframerate(16000)
                    wf.writeframes((seg_samples * 32768).astype(np.int16).tobytes())
                try:
                    result = recognizer.transcribe(tmp_wav.name)
                    text = result.get("text", "").strip()
                finally:
                    os.unlink(tmp_wav.name)
            else:
                # sherpa-onnx OfflineRecognizer 用 create_stream
                stream = recognizer.create_stream()
                stream.accept_waveform(16000, seg_samples)
                recognizer.decode_stream(stream)
                text = stream.result.text.strip()

            if text and text != "<sil>":
                start_sample = segment.start
                start_time = start_sample / 16000.0

                results.append({
                    "text": text,
                    "start": round(start_time, 3),
                    "end": round(start_time + seg_duration, 3),
                    "duration": round(seg_duration, 3),
                    "engine": engine_name,
                })

        vad.pop()

    return results


def run_dual_transcribe_impl(wav_path):
    """双引擎并发转写 — FireRed 原生(:8082) + sherpa-onnx FireRed(:8083) 交叉校验
    两个引擎使用同一个 FireRed 模型但不同推理库，做交叉校验
    """
    import subprocess
    import json as json_mod

    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    duration = len(audio) / sample_rate

    results = {}

    # 引擎1: 本服务 (:8083) 的 firered_native
    if "firered_native" in engines:
        try:
            results["sherpa_native"] = transcribe_firered_native(engines["firered_native"], wav_path)
        except Exception as e:
            results["sherpa_native"] = {"error": str(e), "engine": "sherpa_native"}

    # 引擎2: FireRed 原生服务 (:8082)
    try:
        import urllib.request
        with open(wav_path, 'rb') as f:
            audio_bytes = f.read()
        boundary = '----FormBoundary7MA4YWxkTrZu0gW'
        body = (
            f'--{boundary}\r\n'
            f'Content-Disposition: form-data; name="file"; filename="audio.wav"\r\n'
            f'Content-Type: audio/wav\r\n\r\n'
        ).encode() + audio_bytes + f'\r\n--{boundary}--\r\n'.encode()
        req = urllib.request.Request(
            'http://127.0.0.1:8082/transcribe',
            data=body,
            headers={'Content-Type': f'multipart/form-data; boundary={boundary}'}
        )
        with urllib.request.urlopen(req, timeout=30) as resp:
            firered_result = json_mod.loads(resp.read())
            results["firered_native"] = firered_result
    except Exception as e:
        results["firered_native"] = {"error": str(e), "engine": "firered_native"}

    # 选择最佳结果
    primary = results.get("sherpa_native", {})
    secondary = results.get("firered_native", {})

    primary_text = primary.get("text", "") if isinstance(primary, dict) else ""
    secondary_text = secondary.get("text", "") if isinstance(secondary, dict) else ""

    # 启发式选择：优先用 firered_native（8082），如果空则用 sherpa_native
    if secondary_text:
        best_text = secondary_text
        best_engine = "firered_native"
    elif primary_text:
        best_text = primary_text
        best_engine = "sherpa_native"
    else:
        best_text = ""
        best_engine = "none"

    return {
        "text": best_text,
        "duration": duration,
        "engine": best_engine,
        "primary": primary,
        "secondary": secondary,
    }


def extract_embedding_impl(extractor, wav_path):
    """从音频文件提取声纹嵌入"""
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    stream = extractor.create_stream()
    stream.accept_waveform(sample_rate, audio)
    stream.input_finished()
    
    if not extractor.is_ready(stream):
        return None
    
    embedding = extractor.compute(stream)
    return np.array(embedding, dtype=np.float32)


# === 三引擎交叉校验 ===

PARAFORMER_DIR = os.path.expanduser("~/.cache/sherpa-models/paraformer")

# === Qwen3-ASR 引擎（主引擎：高精度中文识别） ===
# v3: 单引擎架构，统一使用 Qwen3-ASR-1.7B (q4_k)
# 1.7B 为主力（专有名词识别更准），0.6B 保留在 qwen3-asr-model 作为 fallback
# v2.3.3: 模型路径从 TeleAgent .temp 临时目录迁移到 ~/Library/Application Support/PanNote/models/
#         .temp 随时可能被清理，应用私有目录稳定可靠
QWEN3_ASR_PROJECT = os.path.expanduser(
    "~/Library/Application Support/PanNote/models/Qwen3-ASR-GGUF"
)
QWEN3_ASR_MODEL_DIR = os.path.expanduser(
    "~/Library/Application Support/PanNote/models/qwen3-asr-1.7b"
)
QWEN3_ASR_MODEL_DIR_FALLBACK = os.path.expanduser(
    "~/Library/Application Support/PanNote/models/qwen3-asr-0.6b"
)

# 引擎2/3 合并参数
MEDIUM_MIN_DUR = 10.0   # 引擎2：合并段最短时长（秒）
MEDIUM_MAX_DUR = 60.0   # 引擎2：合并段最长时长（秒）——从30调到60，减少段数约一半
FULL_MIN_DUR = 30.0     # 引擎3：合并段最短时长（秒）
FULL_MAX_DUR = 120.0    # 引擎3：合并段最长时长（秒）

# 口语化重复词词典
ORAL_REPEAT_MAP = {
    "对对对": "对", "对对对对": "对", "对对": "对",
    "是是是": "是", "是是": "是",
    "好好好": "好", "好好": "好",
    "嗯嗯嗯": "嗯", "嗯嗯": "嗯",
    "就是就是": "就是", "就是就是就是": "就是",
    "那个那个": "那个", "那个那个那个": "那个",
    "然后然后": "然后",
    "这这": "这", "这个这个": "这个",
    "我我": "我", "我们我们": "我们",
}

# === 热词场景词库（v3: 场景热词自进化第一层预制词库） ===
# 通过 Qwen3-ASR context biasing 注入，提升专有名词识别率
HOTWORD_SCENES = {
    "通用": [],
    "财务": ["增值税", "企业所得税", "个人所得税", "差旅费", "报销单", "发票", "审批流",
             "预算", "决算", "财务报表", "应收账款", "应付账款", "现金流量", "资产负债率",
             "税务筹划", "审计", "内控", "报销", "预算是"],
    "技术": ["API", "Docker", "Kubernetes", "微服务", "数据库", "后端", "前端", "部署",
             "上线", "灰度", "回滚", "故障", "监控", "告警", "网关", "中间件", "缓存",
             "队列", "异步", "并发", "分布式", "容器", "镜像", "流水线", "代码评审"],
    "游戏": ["植物大战僵尸", "豌豆射手", "向日葵", "太阳花", "能量豆", "僵尸", "坚果墙",
             "樱桃炸弹", "寒冰射手", "双发豌豆", "土豆雷", "大喷菇", "胆小菇", "玉米投手",
             "西瓜投手", "冰豌豆", "宇宙豌豆", "阳光标志", "关卡", "波次"],
    "日常": ["会议室", "投影仪", "签到", "周报", "月报", "例会", "日程", "审批"],
    "医疗": ["血压", "血糖", "心率", "体检", "挂号", "门诊", "住院", "医保", "处方", "CT", "核磁"],
    "法律": ["合同", "协议", "条款", "违约责任", "仲裁", "诉讼", "律师", "公证", "有效期", "甲方", "乙方"],
}

# 场景关键词（用于自动识别场景，第二层）
SCENE_KEYWORDS = {
    "财务": ["报销", "发票", "预算", "财务", "税务", "审计", "成本", "利润", "账"],
    "技术": ["代码", "部署", "接口", "服务器", "数据库", "开发", "测试", "bug", "系统", "版本"],
    "游戏": ["僵尸", "植物", "关卡", "波", "阳光", "射手", "炸弹", "植物大战"],
    "医疗": ["医生", "患者", "药", "医院", "症状", "检查", "治疗"],
    "法律": ["合同", "条款", "违约", "法律", "诉讼", "仲裁", "签字"],
}


def get_hotword_context(scene=None):
    """获取热词 context 字符串（Qwen3-ASR context biasing 用）

    如果指定场景，返回该场景热词 + 通用热词；
    否则返回所有场景热词（默认场景）。
    """
    if scene and scene in HOTWORD_SCENES:
        words = list(HOTWORD_SCENES.get("通用", [])) + list(HOTWORD_SCENES[scene])
    else:
        # 默认返回所有场景的关键热词（去重，限制数量避免影响解码速度）
        words = []
        for s, ws in HOTWORD_SCENES.items():
            words.extend(ws)
    # 去重 + 限制条数（context 过长会拖慢解码）
    seen = set()
    deduped = []
    for w in words:
        if w not in seen:
            seen.add(w)
            deduped.append(w)
    return " ".join(deduped[:50])


def init_paraformer():
    """初始化 Paraformer 引擎（三引擎架构的引擎3）"""
    model_path = os.path.join(PARAFORMER_DIR, "model.int8.onnx")
    tokens_path = os.path.join(PARAFORMER_DIR, "tokens.txt")

    if not os.path.exists(model_path) or not os.path.exists(tokens_path):
        print(f"[Paraformer] 模型文件缺失: {model_path}")
        return None

    try:
        import sherpa_onnx
        recognizer = sherpa_onnx.OfflineRecognizer.from_paraformer(
            paraformer=model_path,
            tokens=tokens_path,
            num_threads=4,
            sample_rate=16000,
            feature_dim=80,
            decoding_method="greedy_search",
            debug=False,
            provider="cpu",
        )
        print(f"[Paraformer] 加载成功")
        return recognizer
    except Exception as e:
        print(f"[Paraformer] 加载失败: {e}")
        return None


def init_qwen3_asr(model_dir=None, label=None, n_ctx=4096, chunk_size=40.0):
    """初始化 Qwen3-ASR 引擎（主引擎）

    使用 ONNX Encoder + GGUF Decoder (llama.cpp)，纯 CPU 推理。
    v4: 双引擎架构：
      - 精转引擎：1.7B (q4_k)，优先加载，失败 fallback 0.6B
      - 快速引擎：0.6B，准实时段转写用
    model_dir/label 指定时只尝试该模型（供快速引擎用）。
    初始化耗时约 3-6 秒，做成全局单例只加载一次。
    """
    # 检查项目目录
    if not os.path.isdir(QWEN3_ASR_PROJECT):
        print(f"[Qwen3-ASR] 项目目录不存在: {QWEN3_ASR_PROJECT}")
        return None

    # 选择要尝试的模型列表
    if model_dir and label:
        candidates = [(model_dir, label)]
    else:
        # 精转引擎：1.7B 优先 → 0.6B fallback（2026-08-30: b9733 dylib 测试通过，1.7B 稳定且专有名词识别更准）
        candidates = [
            (QWEN3_ASR_MODEL_DIR, "1.7B"),
            (QWEN3_ASR_MODEL_DIR_FALLBACK, "0.6B"),
        ]

    for mdir, lbl in candidates:
        model_files = [
            os.path.join(mdir, "qwen3_asr_encoder_frontend.int4.onnx"),
            os.path.join(mdir, "qwen3_asr_encoder_backend.int4.onnx"),
            os.path.join(mdir, "qwen3_asr_llm.q4_k.gguf"),
        ]
        missing = [f for f in model_files if not os.path.exists(f)]
        if missing:
            print(f"[Qwen3-ASR] {lbl} 模型文件缺失: {[os.path.basename(m) for m in missing]}")
            continue

        try:
            # 将项目目录加入 sys.path
            if QWEN3_ASR_PROJECT not in sys.path:
                sys.path.insert(0, QWEN3_ASR_PROJECT)

            # v2.4.2: 关掉 qwen_asr_gguf 的文件日志——模块 import 时默认 INFO 落盘
            # (llama.cpp 初始化 sched/graph 全量 dump，单次启动 ~335MB latest.log，磁盘紧张雪上加霜)。
            # 降级 CRITICAL + log_file=None：引擎初始化细节仅控制台可见，不再写盘。
            from qwen_asr_gguf import setup_logging as _gguf_setup_logging
            _gguf_setup_logging(level=50, log_file=None)  # 50 = CRITICAL

            from qwen_asr_gguf.inference import QwenASREngine, ASREngineConfig

            config = ASREngineConfig(
                model_dir=mdir,
                encoder_frontend_fn="qwen3_asr_encoder_frontend.int4.onnx",
                encoder_backend_fn="qwen3_asr_encoder_backend.int4.onnx",
                llm_fn="qwen3_asr_llm.q4_k.gguf",
                onnx_provider="CPU",
                llm_use_gpu=False,
                n_ctx=n_ctx,
                chunk_size=chunk_size,
                memory_num=1,
                enable_aligner=False,
                verbose=False,
                llm_threads=6,       # 2026-09-04: 0.6B 精转用 6 线程（encoder 4 + decoder 6 = 10，留 2 给其他）
                llm_n_batch=2048,    # 2026-09-04: 40s chunk 音频token约800，2048 足够
            )
            engine = QwenASREngine(config)
            print(f"[Qwen3-ASR] {lbl} 加载成功 ({mdir})")
            return engine
        except Exception as e:
            print(f"[Qwen3-ASR] {lbl} 加载失败: {e}")
            import traceback
            traceback.print_exc()

    return None


def transcribe_qwen3_asr(engine, wav_path, context=None):
    """用 Qwen3-ASR 转写完整音频文件

    P7: 转写前降噪 + 精转锁（1.7B，默认 ASR_INFER_LOCK）防并发崩。
    context: 可选热词字符串（Qwen3-ASR 原生 context biasing）
    """
    import wave

    # 降噪（临时文件用完即删）
    denoised_path = denoise_wav(wav_path)
    try:
        return _transcribe_qwen3_asr_core(engine, denoised_path, context, lock=ASR_INFER_LOCK)
    finally:
        if denoised_path != wav_path and os.path.exists(denoised_path):
            os.unlink(denoised_path)


def _transcribe_qwen3_asr_core(engine, wav_path, context=None, lock=None):
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    duration = len(audio) / sample_rate

    # 2026-09-11 防幻觉：静音段不喂模型。
    # 根因：麦克风权限被 macOS 静默拒绝时 cpal 采集到全零数据，
    # Qwen3-ASR 对静音输入会产生幻觉文本（实测输出 "15%" /
    # "You are a helpful assistant."）。
    # 判据：RMS < 1e-4（-80dB，比正常语音低 40dB 以上）即视为静音。
    if duration > 0:
        rms = float(np.sqrt(np.mean(audio ** 2)))
        if rms < 1e-4:
            return {
                "text": "",
                "duration": duration,
                "engine": "qwen3_asr",
                "silent": True,
                "rms": round(rms, 8),
            }

    kwargs = {"language": "Chinese"}
    if context:
        kwargs["context"] = context

    # 在指定锁内执行（llama context 非线程安全）
    result = run_asr_locked(engine.transcribe, wav_path, lock=lock or ASR_INFER_LOCK, **kwargs)

    return {
        "text": result.text.strip(),
        "duration": duration,
        "engine": "qwen3_asr",
    }


def transcribe_qwen3_asr_quick(engine, wav_path, context=None):
    """P8: 0.6B 快速引擎转写（准实时段转写用）

    与精转引擎同样的降噪，但走独立快速锁（ASR_QUICK_LOCK），
    精转（1.7B）进行时不阻塞准实时段转写。
    10s 段在 0.6B 下约 2-3s 出字，实时感好。
    """
    import wave

    denoised_path = denoise_wav(wav_path)
    try:
        return _transcribe_qwen3_asr_core(engine, denoised_path, context, lock=ASR_QUICK_LOCK)
    finally:
        if denoised_path != wav_path and os.path.exists(denoised_path):
            os.unlink(denoised_path)


def transcribe_qwen3_asr_quick_pool(wav_path, context=None):
    """2026-09-03: 双实例负载均衡版快速转写

    从 ASR_QUICK_ENGINES 轮询取一个空闲实例（各实例持独立锁），
    并发度×2 解决 10s 段积压。返回与单实例相同结构。
    """
    global ASR_QUICK_ROUND_ROBIN
    import wave

    # 选实例：轮询游标（锁只保护计数）
    with ASR_QUICK_LOCK:
        if not ASR_QUICK_ENGINES:
            # 双实例都不可用，回退主引擎（1.7B）
            engine = qwen3_asr_engine
            if engine is None:
                return {"text": "", "duration": 0.0, "engine": "qwen3_asr_quick", "error": "no engine"}
            return _transcribe_qwen3_asr_core(engine, wav_path, context, lock=ASR_INFER_LOCK)
        idx = ASR_QUICK_ROUND_ROBIN % len(ASR_QUICK_ENGINES)
        ASR_QUICK_ROUND_ROBIN += 1
    engine, elock = ASR_QUICK_ENGINES[idx]

    denoised_path = denoise_wav(wav_path)
    try:
        return _transcribe_qwen3_asr_core(engine, denoised_path, context, lock=elock)
    finally:
        if denoised_path != wav_path and os.path.exists(denoised_path):
            os.unlink(denoised_path)


def transcribe_paraformer(recognizer, wav_path):
    """用 Paraformer 转写"""
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    stream = recognizer.create_stream()
    stream.accept_waveform(sample_rate, audio)
    recognizer.decode_stream(stream)

    return {
        "text": stream.result.text,
        "duration": len(audio) / sample_rate,
        "engine": "paraformer",
    }


def transcribe_paraformer_realtime(wav_path):
    """v2.5.0: 实时段 Paraformer 转写——RMS 静音判定 + 转写 + 标点内联 + 填充词过滤
    返回结构与 quick_pool 一致（text/duration/engine，静音时 silent=True）"""
    import wave
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0
    duration = len(audio) / sample_rate
    # 防幻觉静音判定（与 0.6B 同判据：RMS < 1e-4）
    if duration > 0:
        rms = float(np.sqrt(np.mean(audio ** 2)))
        if rms < 1e-4:
            return {"text": "", "duration": duration, "engine": "paraformer_rt", "silent": True, "rms": round(rms, 8)}
    stream = PARAFORMER_RT.create_stream()
    stream.accept_waveform(sample_rate, audio)
    PARAFORMER_RT.decode_stream(stream)
    text = stream.result.text.strip()
    # 标点内联 + 填充词过滤（与 /punctuate 同管线，前端零改动）
    if text and punct_model is not None:
        text = punct_model.add_punctuation(text)
        text = filter_fillers(text)
    return {"text": text, "duration": duration, "engine": "paraformer_rt"}


def _vad_segment(audio, sample_rate):
    """用 Silero VAD 把音频切成语音段，返回 [(start_sample, end_sample), ...]"""
    import sherpa_onnx
    vad = sherpa_onnx.VoiceActivityDetector(vad_config, buffer_size_in_seconds=300)
    chunk_size = 1024
    for i in range(0, len(audio), chunk_size):
        chunk = audio[i:i + chunk_size]
        if len(chunk) < chunk_size:
            chunk = np.pad(chunk, (0, chunk_size - len(chunk)))
        vad.accept_waveform(chunk)
    try:
        vad.flush()
    except Exception:
        pass

    segments = []
    while not vad.empty():
        seg = vad.front
        segments.append((seg.start, seg.start + len(seg.samples)))
        vad.pop()
    return segments


def _merge_vad_segments(vad_segments, sample_rate, min_dur, max_dur):
    """把 VAD 检测到的碎片段合并到 min_dur~max_dur 范围

    策略：贪心合并，累加时长直到达到 min_dur，超过 max_dur 就切。
    合并时记录原始段的起止时间。
    """
    merged = []
    buf_start = None
    buf_end = None
    buf_duration = 0.0

    for start, end in vad_segments:
        seg_dur = (end - start) / sample_rate
        if buf_start is None:
            buf_start = start
            buf_end = end
            buf_duration = seg_dur
        elif buf_duration + seg_dur > max_dur:
            # 超过上限，保存上一段
            merged.append((buf_start, buf_end))
            buf_start = start
            buf_end = end
            buf_duration = seg_dur
        else:
            buf_end = end
            buf_duration += seg_dur

        # 达到下限且下一段会超上限时，也可以提前切
        # 简化处理：只判断是否超过上限
        if buf_duration >= max_dur:
            merged.append((buf_start, buf_end))
            buf_start = None
            buf_end = None
            buf_duration = 0.0

    if buf_start is not None:
        merged.append((buf_start, buf_end))

    # 过滤过短的段
    merged = [(s, e) for s, e in merged if (e - s) / sample_rate >= 0.3]
    return merged


def run_medium_transcribe_impl(wav_path):
    """引擎2：VAD中粒度分段 + FireRed逐段转写

    返回: [{"text": ..., "start": ..., "end": ..., "engine": "firered_native"}, ...]
    """
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    vad_segs = _vad_segment(audio, sample_rate)
    if not vad_segs:
        return [{"text": "", "start": 0, "end": 0, "engine": "firered_native"}]

    merged_segs = _merge_vad_segments(vad_segs, sample_rate, MEDIUM_MIN_DUR, MEDIUM_MAX_DUR)

    # 选择转写引擎（优先 FireRed Native）
    use_firered_native = False
    recognizer = None
    engine_name = "none"
    if "firered_native" in engines:
        recognizer = engines["firered_native"]
        engine_name = "firered_native"
        use_firered_native = True
    elif "sensevoice" in engines:
        recognizer = engines["sensevoice"]
        engine_name = "sensevoice"
    elif "paraformer" in engines:
        recognizer = engines["paraformer"]
        engine_name = "paraformer"
    else:
        return [{"text": "", "start": 0, "end": 0, "error": "no engine available"}]

    results = []
    for seg_start, seg_end in merged_segs:
        seg_samples = audio[seg_start:seg_end]
        start_time = seg_start / sample_rate
        end_time = seg_end / sample_rate
        seg_duration = end_time - start_time

        text = _transcribe_segment(recognizer, seg_samples, sample_rate,
                                    use_firered_native, engine_name)

        if text and text != "<sil>":
            results.append({
                "text": text,
                "start": round(start_time, 3),
                "end": round(end_time, 3),
                "duration": round(seg_duration, 3),
                "engine": engine_name,
            })

    return results


def run_full_transcribe_impl(wav_path, context=None):
    """主转写：VAD 分段 + Qwen3-ASR 长段精转

    v3: 单引擎架构，统一用 Qwen3-ASR（1.7B 主力 / 0.6B fallback）。
    用 VAD 分段后合并到 30-120 秒长段，用 Qwen3-ASR 转写。
    P7: 转写前先降噪（afftdn），推理走全局锁防并发崩。
    2026-09-04: 优先用 0.6B 快速引擎做精转（RTF~0.27，55分钟约15分钟完成），
    1.7B 纯 CPU RTF~5-10 不可接受，仅保留为 fallback。
    context: 可选热词字符串（context biasing）。

    返回: [{"text": ..., "start": ..., "end": ..., "engine": "qwen3_asr"}, ...]
    """
    # 2026-09-04: 优先用 0.6B 引擎做精转
    # 2026-09-23: 精转固定用池实例[0]，并持其专属推理锁。
    #   原实现拿 ASR_QUICK_LOCK（仅轮询计数锁）当推理锁，双实例后会与
    #   quick 池并发复用实例0（llama context 非线程安全 → GGML_ASSERT 崩溃）。
    if ASR_QUICK_ENGINES:
        engine, use_lock = ASR_QUICK_ENGINES[0]
    elif qwen3_asr_engine_quick is not None:
        engine, use_lock = qwen3_asr_engine_quick, ASR_INFER_LOCK
    else:
        engine, use_lock = qwen3_asr_engine, ASR_INFER_LOCK
    if engine is None:
        return [{"text": "", "start": 0, "end": 0, "error": "qwen3_asr not loaded"}]

    # P7: 降噪（不修改原始文件，临时文件用完即删）
    denoised_path = denoise_wav(wav_path)
    try:
        return _run_full_transcribe_core(denoised_path, context, engine, use_lock)
    finally:
        if denoised_path != wav_path and os.path.exists(denoised_path):
            os.unlink(denoised_path)


def _run_full_transcribe_core(wav_path, context=None, engine=None, lock=None):
    """full_transcribe 核心逻辑（处理降噪后的音频）
    2026-09-04: engine/lock 可传入，优先用 0.6B
    """
    if engine is None:
        engine = qwen3_asr_engine
    if lock is None:
        lock = ASR_INFER_LOCK
    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    vad_segs = _vad_segment(audio, sample_rate)
    if not vad_segs:
        return [{"text": "", "start": 0, "end": 0, "engine": "qwen3_asr"}]

    merged_segs = _merge_vad_segments(vad_segs, sample_rate, FULL_MIN_DUR, FULL_MAX_DUR)

    results = []
    for seg_start, seg_end in merged_segs:
        seg_samples = audio[seg_start:seg_end]
        start_time = seg_start / sample_rate
        end_time = seg_end / sample_rate
        seg_duration = end_time - start_time

        # Qwen3-ASR 需要文件路径，写临时 WAV
        import tempfile
        tmp_wav = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
        tmp_wav.close()
        try:
            with wave.open(tmp_wav.name, "w") as wf:
                wf.setnchannels(1)
                wf.setsampwidth(2)
                wf.setframerate(16000)
                wf.writeframes((seg_samples * 32768).astype(np.int16).tobytes())
            kwargs = {"language": "Chinese"}
            if context:
                kwargs["context"] = context
            # P7: 全局锁内执行（llama context 非线程安全）
            result = run_asr_locked(engine.transcribe, tmp_wav.name, lock=lock, **kwargs)
            text = result.text.strip()
        finally:
            os.unlink(tmp_wav.name)

        if text and text != "<sil>":
            results.append({
                "text": text,
                "start": round(start_time, 3),
                "end": round(end_time, 3),
                "duration": round(seg_duration, 3),
                "engine": "qwen3_asr",
            })

    return results


def _transcribe_segment(recognizer, seg_samples, sample_rate, use_firered_native, engine_name):
    """转写一段音频，返回纯文本"""
    if use_firered_native:
        import tempfile
        tmp_wav = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
        tmp_wav.close()
        try:
            with wave.open(tmp_wav.name, "w") as wf:
                wf.setnchannels(1)
                wf.setsampwidth(2)
                wf.setframerate(16000)
                wf.writeframes((seg_samples * 32768).astype(np.int16).tobytes())
            result = recognizer.transcribe(tmp_wav.name)
            return result.get("text", "").strip()
        finally:
            os.unlink(tmp_wav.name)
    else:
        stream = recognizer.create_stream()
        stream.accept_waveform(sample_rate, seg_samples)
        recognizer.decode_stream(stream)
        return stream.result.text.strip()


# === LLM 语义融合配置 ===
OLLAMA_URL = "http://127.0.0.1:11434/api/generate"
OLLAMA_MODEL = "qwen3-4b-32k"
LLM_FUSE_BATCH = 20  # 每次送LLM处理的选择题数（选择题token小，可大批次）
LLM_TIMEOUT = 120    # LLM超时秒数（选择题模式，120s够）
SIM_THRESHOLD = 0.85  # 相似度≥此值视为无差异，跳过LLM


def _call_ollama_choice(choices_batch):
    """调用本地 Ollama 做差异选择题（不是生成全文）

    Args:
        choices_batch: [{"idx": 0, "context": "...", "options": {"A": "...", "B": "..."}}]
                     其中 A=Qwen3底本(已验证术语识别率最高)，B=FireRed差异版
    Returns:
        {idx: "A"或"B"}  失败时返回 {}
    """
    import urllib.request
    import re

    lines = []
    for item in choices_batch:
        lines.append(f"--- 选择 {item['idx']} ---")
        lines.append(f"上下文: {item.get('context', '')}")
        for key in sorted(item['options'].keys()):
            lines.append(f"{key}. {item['options'][key]}")
        lines.append("")
    batch_content = "\n".join(lines)

    prompt = (
        "/no_think\n"
        "你是语音转写纠错助手。以下是同一段录音不同引擎的差异片段。\n"
        "引擎A(Qwen3)术语识别率最高，通常最准确；引擎B(FireRed)可能有术语错误。\n"
        "请逐个选择最准确的版本。只输出JSON数组: [{\"idx\": 0, \"choice\": \"A\"}]\n"
        "不要输出其他内容。\n\n"
        f"{batch_content}"
    )

    payload = json.dumps({
        "model": OLLAMA_MODEL,
        "prompt": prompt,
        "stream": False,
        "options": {"temperature": 0.1, "num_predict": 512},
        "think": False,
    }).encode()

    try:
        req = urllib.request.Request(
            OLLAMA_URL,
            data=payload,
            headers={"Content-Type": "application/json"},
        )
        with urllib.request.urlopen(req, timeout=LLM_TIMEOUT) as resp:
            result = json.loads(resp.read())
            text = result.get("response", "").strip()

            try:
                choice_list = json.loads(text)
            except json.JSONDecodeError:
                match = re.search(r'```(?:json)?\s*(\[.*\])\s*```', text, re.DOTALL)
                if match:
                    choice_list = json.loads(match.group(1))
                else:
                    match = re.search(r'\[.*\]', text, re.DOTALL)
                    if match:
                        choice_list = json.loads(match.group(0))
                    else:
                        print(f"[CrossValidate] LLM返回非JSON: {text[:200]}")
                        return {}

            return {item["idx"]: item["choice"].strip() for item in choice_list}
    except Exception as e:
        print(f"[CrossValidate] LLM选择题失败: {e}")
        return {}


def run_cross_validate_impl(engine1_segments, engine2_segments, engine3_segments):
    """三引擎交叉校验 — Qwen3底本 + 差异选择题架构

    核心思路：
      1. 以 Qwen3-ASR (engine3) 为底本——已验证CER最低、术语识别率最高(13/13)
      2. 对每个Qwen3段，找引擎1/2对应时间区间的文本
      3. 代码层用difflib标差异：相似度≥SIM_THRESHOLD → 无差异，直接用Qwen3
      4. 有差异段：整理成A/B选择题发给4b LLM（A=Qwen3底本, B=FireRed差异版）
      5. LLM做选择题（不是生成题），token量降一个量级
      6. LLM超时 → 全用Qwen3底本（fallback最准）

    输入：三份转写结果，每份是 [{"text": ..., "start": ..., "end": ...}, ...]
    输出：[{"text": ..., "start": ..., "end": ..., "engine": "cross_validated"}, ...]
    """
    from difflib import SequenceMatcher

    def text_similarity(a, b):
        if not a or not b:
            return 0.0
        return SequenceMatcher(None, a, b).ratio()

    def find_overlapping(segments, start, end, min_overlap=0.3):
        """从 segments 中找与 [start, end] 时间重叠最大的段，返回其文本
        
        注意：只返回最佳匹配段，不拼接多段——避免长段被反复匹配导致文本重复。
        """
        if not segments:
            return ""
        best_text = ""
        best_overlap = 0.0
        for seg in segments:
            seg_start = seg.get("start", 0)
            seg_end = seg.get("end", 0)
            overlap = min(end, seg_end) - max(start, seg_start)
            if overlap > 0:
                overlap_ratio = overlap / max(end - start, 0.001)
                if overlap_ratio >= min_overlap and overlap > best_overlap:
                    best_overlap = overlap
                    best_text = seg.get("text", "")
        return best_text

    # === 以 Qwen3-ASR (engine3) 为底本 ===
    base_segments = engine3_segments if engine3_segments else (engine2_segments if engine2_segments else engine1_segments)
    if not base_segments:
        return []

    results = []
    choices_pending = []  # 需要LLM做选择题的段
    skipped = 0  # 无差异直接采用计数

    for base_seg in base_segments:
        start = base_seg.get("start", 0)
        end = base_seg.get("end", 0)

        # Qwen3底本文本
        text_qwen = base_seg.get("text", "").strip()

        # 找引擎1/2对应文本
        text1 = find_overlapping(engine1_segments, start, end).strip()
        text2 = find_overlapping(engine2_segments, start, end).strip()

        # 规则预去噪
        text_qwen = _dedup_oral_repeat(text_qwen) if text_qwen else ""
        text1 = _dedup_oral_repeat(text1) if text1 else ""
        text2 = _dedup_oral_repeat(text2) if text2 else ""

        if not text_qwen:
            # Qwen3底本为空，取最长
            texts = [t for t in [text1, text2] if t]
            if texts:
                results.append({
                    "text": max(texts, key=len),
                    "start": start, "end": end,
                    "engine": "cross_validated",
                })
            continue

        result_index = len(results)

        # 计算Qwen3底本与引擎1/2的相似度
        sim1 = text_similarity(text_qwen, text1) if text1 else 0.0
        sim2 = text_similarity(text_qwen, text2) if text2 else 0.0
        max_other_sim = max(sim1, sim2)

        if max_other_sim >= SIM_THRESHOLD or (not text1 and not text2):
            # 无显著差异（或没有其他引擎结果可比），直接用Qwen3底本
            results.append({
                "text": text_qwen,
                "start": start, "end": end,
                "engine": "cross_validated",
            })
            skipped += 1
        else:
            # 有差异 → 收集差异选项
            options = {"A": text_qwen}  # A=Qwen3底本
            if text1 and text1 != text_qwen:
                options["B"] = text1  # B=FireRed 30s
            if text2 and text2 != text_qwen and text2 != text1:
                # 如果B已被text1占用，用C
                next_key = "C" if "B" in options else "B"
                options[next_key] = text2

            # 上下文（取前一段的后40字，帮助LLM理解语境）
            context = results[-1]["text"][-40:] if results else ""

            choices_pending.append({
                "idx": result_index,
                "context": context,
                "options": options,
                "fallback": text_qwen,  # LLM超时用Qwen3底本
            })

            # 先放Qwen3底本作为fallback
            results.append({
                "text": text_qwen,
                "start": start, "end": end,
                "engine": "cross_validated",
            })

    print(f"[CrossValidate] Qwen3底本: {len(base_segments)}段, "
          f"无差异跳过: {skipped}, 需LLM选择: {len(choices_pending)}", flush=True)

    # === LLM做选择题（批量） ===
    if choices_pending:
        batches = (len(choices_pending) + LLM_FUSE_BATCH - 1) // LLM_FUSE_BATCH
        print(f"[CrossValidate] 分 {batches} 批选择题 (每批≤{LLM_FUSE_BATCH}个)",
              flush=True)

        for batch_start in range(0, len(choices_pending), LLM_FUSE_BATCH):
            batch = choices_pending[batch_start:batch_start + LLM_FUSE_BATCH]
            choices = _call_ollama_choice(batch)

            applied = 0
            for item in batch:
                ri = item["idx"]
                choice = choices.get(ri, "")
                if choice in item["options"]:
                    results[ri]["text"] = item["options"][choice]
                    applied += 1
                # LLM没选或选错 → 保持Qwen3底本（fallback）

            print(f"[CrossValidate] 批次 {batch_start // LLM_FUSE_BATCH + 1}: "
                  f"{applied}/{len(batch)} 个选择采纳", flush=True)

    # === 后处理：去重 ===
    seen_texts = set()
    deduped = []
    for seg in results:
        text_key = seg.get("text", "")[:80]
        if text_key and text_key in seen_texts:
            continue
        seen_texts.add(text_key)
        deduped.append(seg)

    if len(deduped) < len(results):
        print(f"[CrossValidate] 去重: {len(results)} -> {len(deduped)} 段", flush=True)

    return deduped


def _dedup_oral_repeat(text):
    """去除口语化重复"""
    for pattern, replacement in ORAL_REPEAT_MAP.items():
        text = text.replace(pattern, replacement)

    # 去除连续重复的句子（完整句子出现两次的，只保留一次）
    import re
    sentences = re.split(r'([。！？\n])', text)
    seen = set()
    result = []
    i = 0
    while i < len(sentences):
        s = sentences[i].strip()
        if i + 1 < len(sentences) and sentences[i + 1] in "。！？\n":
            full_sentence = s + sentences[i + 1]
            key = full_sentence.strip()
            if key and key not in seen:
                seen.add(key)
                result.append(full_sentence)
            i += 2
        else:
            if s:
                result.append(s)
            i += 1

    return "".join(result)


# === 口语填充词过滤 ===
# 核心原则：只删"无实义的填充词"，保留"有实义的代词/副词"
# 判定逻辑：填充词后面紧跟的是标点/停顿/另一个填充词 → 删
#           填充词后面紧跟的是名词/动词 → 保留（此时是代词用法）

import re as _re

# 填充词列表（按优先级：先处理组合，再处理单字）
_FILLER_PATTERNS = [
    # 1. 句间孤立的填充词组合："就是然后"、"那个那个就是" 等
    #    匹配：填充词之间用逗号/顿号连接的片段
    (_re.compile(r'(?:嗯|啊|呃|嘛|吧|哈|哎)(?=[，。！？,;]|$)'), ''),  # 句末语气词

    # 2. "这个""那个"做填充词：后面跟标点或另一个填充词 → 删
    #    "这个，" / "那个。" / "就是，" / "然后，"
    (_re.compile(r'(?:这个|那个|就是|然后|其实|基本(?:上)?|大概|可能|好像|那么)(?=[，。！？,;]|$)'), ''),

    # 3. 句首填充词："然后..." "就是..." 开头
    (_re.compile(r'^(?:然后|就是|那个|这个|其实|那么)[，,]?\s*'), ''),

    # 4. 连续填充词："就是那个" "然后这个" 等（中间无实义词）
    #    只有当后面也跟着标点时才删
    (_re.compile(r'(?:这个|那个|就是|然后|其实)+[，,](?:这个|那个|就是|然后|其实)+[，,]?'), ''),

    # 5. 句中"。，"杂标点清理（Qwen3-ASR 常见副产物）
    (_re.compile(r'。，'), '。'),
    (_re.compile(r',,'), ','),

    # 6. 结巴/重复单字："我我" "这这" (OREAL_REPEAT_MAP 已处理组合词，这里补单字)
    (_re.compile(r'(.)\1{2,}'), r'\1'),
]

# 有实义用法保护名单：这些词组中的"这个/那个/就是"不删
_PROTECTED_PATTERNS = _re.compile(
    r'(?:那个|这个)(?:事情|事儿|问题|项目|文件|方向|方面|环节|阶段|时候|地方|会议|东西|意思|点|人|事)'
    r'|(?:那个|这个)(?:[^\s，。]{0,3}(?:问题|事情|项目|文件|方向|方面|环节|阶段|时候|地方|会议))'
    r'|(?:就是[说讲])'
    r'|(?:那个|这个)(?:叫什么|是|的|了)'
)


def filter_fillers(text):
    """过滤口语填充词，保留实义用法

    策略：
    1. 先保护有实义的词组（"那个问题""就是说"等）
    2. 正则删除孤立填充词（后面跟标点的）
    3. 清理多余标点
    4. 恢复被保护的词组
    """
    if not text or len(text) < 2:
        return text

    import re

    # 第1步：找出所有实义用法，用占位符保护
    protected = []
    def _protect(m):
        protected.append(m.group(0))
        return '\x00PROTECT%d\x00' % (len(protected) - 1)

    text = _PROTECTED_PATTERNS.sub(_protect, text)

    # 第2步：逐条应用填充词过滤
    for pattern, replacement in _FILLER_PATTERNS:
        text = pattern.sub(replacement, text)

    # 第3步：恢复被保护的内容
    for i, original in enumerate(protected):
        text = text.replace('\x00PROTECT%d\x00' % i, original)

    # 第4步：清理多余空格和连续标点
    text = re.sub(r'\s{2,}', ' ', text)
    text = re.sub(r'[，,]{2,}', '，', text)
    text = re.sub(r'[。]{2,}', '。', text)
    text = re.sub(r'[。][，,]', '。', text)    # 。， → 。
    text = re.sub(r'[，,][。]', '。', text)    # ，。 → 。
    text = re.sub(r'[、][，,。]', '、', text)   # 、， → 、
    text = re.sub(r'^[，,。、\s]+', '', text)  # 开头标点

    return text.strip()


# === 填充词过滤结束 ===
    """去除口语化重复"""
    for pattern, replacement in ORAL_REPEAT_MAP.items():
        text = text.replace(pattern, replacement)

    # 去除连续重复的句子（完整句子出现两次的，只保留一次）
    import re
    sentences = re.split(r'([。！？\n])', text)
    seen = set()
    result = []
    i = 0
    while i < len(sentences):
        s = sentences[i].strip()
        if i + 1 < len(sentences) and sentences[i + 1] in "。！？\n":
            full_sentence = s + sentences[i + 1]
            key = full_sentence.strip()
            if key and key not in seen:
                seen.add(key)
                result.append(full_sentence)
            i += 2
        else:
            if s:
                result.append(s)
            i += 1

    return "".join(result)


# === HTTP Handlers ===


def init_diarizer():
    """初始化说话人分离模型"""
    if not os.path.exists(SEG_MODEL) or not os.path.exists(EMBED_MODEL):
        print(f"[Diarizer] 模型文件缺失")
        return None
    try:
        import sherpa_onnx

        seg_config = sherpa_onnx.OfflineSpeakerSegmentationPyannoteModelConfig(
            model=SEG_MODEL,
            # P5.6 性能优化：默认 0.25 太慢（6.5min 音频 ~150s），0.5 加速 2 倍，精度影响可接受
            window_shift_ratio=0.5,
        )
        embedding_config = sherpa_onnx.SpeakerEmbeddingExtractorConfig(
            model=EMBED_MODEL,
            num_threads=2,
            debug=False,
        )
        # P5.7: 会议场景强制分 2 人（pyannote 对中文会议默认倾向单说话人，
        # threshold 0.9/0.5/0.2 都只分出 1 人，改用 num_clusters 强制指定）
        clustering_config = sherpa_onnx.FastClusteringConfig(
            num_clusters=2,
            threshold=0.2,
        )
        config = sherpa_onnx.OfflineSpeakerDiarizationConfig(
            segmentation=sherpa_onnx.OfflineSpeakerSegmentationModelConfig(
                pyannote=seg_config,
            ),
            embedding=embedding_config,
            clustering=clustering_config,
            min_duration_on=0.3,
            min_duration_off=0.5,
        )
        if not config.validate():
            print(f"[Diarizer] 配置验证失败")
            return None
        sd = sherpa_onnx.OfflineSpeakerDiarization(config)
        print(f"[Diarizer] 加载成功, sample_rate={sd.sample_rate}")
        return sd
    except Exception as e:
        print(f"[Diarizer] 加载失败: {e}")
        return None


# 说话人分离时允许的最大说话人数（超出则用声纹 embedding 二次合并）
MAX_SPEAKERS = 4
# 说话人分离最短片段时长（秒），过滤噪声碎片
MIN_SEGMENT_DUR = 1.0


def run_diarization(sd, wav_path):
    """对音频文件执行说话人分离
    返回: {"segments": [{"speaker": 0, "start": 0.5, "end": 6.8}, ...], "num_speakers": 3}
    """
    import wave

    with wave.open(wav_path) as wf:
        sample_rate = wf.getframerate()
        frames = wf.readframes(wf.getnframes())
    audio = np.frombuffer(frames, dtype=np.int16).astype(np.float32) / 32768.0

    result = sd.process(audio)
    num_speakers = result.num_speakers

    # sort_by_start_time() 返回 list[OfflineSpeakerDiarizationSegment]
    # 每个 segment 有 .start, .end, .speaker 属性
    seg_list = result.sort_by_start_time()

    # 重映射 speaker ID 到 0~N-1（处理 clustering 异常 ID 超范围的情况）
    raw_speakers = sorted(set(seg.speaker for seg in seg_list))
    speaker_map = {old: new for new, old in enumerate(raw_speakers)}

    segments = []
    for seg in seg_list:
        speaker_id = speaker_map.get(seg.speaker, 0)
        segments.append({
            "speaker": speaker_id,
            "start": round(seg.start, 3),
            "end": round(seg.end, 3),
        })

    num = len(raw_speakers)
    # P5.5: 自动聚类人数过多时，用声纹 embedding 二次合并到 MAX_SPEAKERS
    if num > MAX_SPEAKERS and embedding_extractor is not None:
        try:
            segments, num = _merge_speakers_by_voiceprint(audio, sample_rate, segments, num)
            print(f"[Diarizer] 声纹二次合并: {len(raw_speakers)} -> {num} 人")
        except Exception as e:
            print(f"[Diarizer] 声纹二次合并失败, 保留原结果: {e}")

    # P5.6: 过滤过短片段（噪声碎片，< 1s 大概率是误检）
    filtered = [s for s in segments if s["end"] - s["start"] >= MIN_SEGMENT_DUR]
    if len(filtered) > 0:
        segments = filtered
        # 过滤后重映射 speaker ID 连续
        final_speakers = sorted(set(s["speaker"] for s in segments))
        final_map = {old: new for new, old in enumerate(final_speakers)}
        for s in segments:
            s["speaker"] = final_map[s["speaker"]]
        num = len(final_speakers)

    return {
        "segments": segments,
        "num_speakers": num,
    }


def _merge_speakers_by_voiceprint(audio, sample_rate, segments, num_speakers):
    """用声纹 embedding 把超过上限的说话人合并到 MAX_SPEAKERS 以内。

    策略：贪心层次合并。
    1. 每个 segment 提取 embedding（太短的段跳过）
    2. 每个说话人组计算平均 embedding 作为组代表向量
    3. 反复合并相似度最高的两个组，直到组数 <= MAX_SPEAKERS
    """
    extractor = embedding_extractor
    if extractor is None:
        return segments, num_speakers

    # 1. 为每个 segment 提取 embedding（仅对长度 >= 0.5s 的段）
    seg_embeddings = []
    for seg in segments:
        start = int(seg["start"] * sample_rate)
        end = int(seg["end"] * sample_rate)
        if end - start < int(0.5 * sample_rate):
            seg_embeddings.append(None)
            continue
        samples = audio[start:end]
        stream = extractor.create_stream()
        stream.accept_waveform(sample_rate, samples)
        stream.input_finished()
        if extractor.is_ready(stream):
            seg_embeddings.append(np.array(extractor.compute(stream), dtype=np.float32))
        else:
            seg_embeddings.append(None)

    # 按说话人分组，累加 embedding 计算组代表向量
    group_embs = {}   # speaker -> np.ndarray (512,)
    group_cnt = {}    # speaker -> int
    group_segs = {}   # speaker -> [seg_index]
    for i, seg in enumerate(segments):
        sp = seg["speaker"]
        group_segs.setdefault(sp, []).append(i)
        if seg_embeddings[i] is None:
            continue
        if sp not in group_embs:
            group_embs[sp] = np.zeros(extractor.dim, dtype=np.float32)
            group_cnt[sp] = 0
        group_embs[sp] += seg_embeddings[i]
        group_cnt[sp] += 1

    # 没有足够 embedding 的组用零向量
    for sp in group_segs:
        if sp not in group_embs:
            group_embs[sp] = np.zeros(extractor.dim, dtype=np.float32)
            group_cnt[sp] = 1

    # 归一化代表向量
    for sp in group_embs:
        n = np.linalg.norm(group_embs[sp])
        if n > 0:
            group_embs[sp] = group_embs[sp] / n

    # 贪心合并：每次找相似度最高的一对，合并后更新代表向量（加权平均）
    while len(group_embs) > MAX_SPEAKERS:
        sp_list = list(group_embs.keys())
        best_pair = None
        best_sim = -1.0
        for i in range(len(sp_list)):
            for j in range(i + 1, len(sp_list)):
                a, b = sp_list[i], sp_list[j]
                sim = float(np.dot(group_embs[a], group_embs[b]))
                if sim > best_sim:
                    best_sim = sim
                    best_pair = (a, b)
        if best_pair is None or best_sim < 0.0:
            # 相似度都为负，把最小组合并
            a, b = sp_list[0], sp_list[1]
            best_pair = (a, b)
        a, b = best_pair
        # 合并 b 到 a（按段数加权平均）
        ca, cb = group_cnt.get(a, 1), group_cnt.get(b, 1)
        total = ca + cb
        group_embs[a] = (group_embs[a] * ca + group_embs[b] * cb) / total
        n = np.linalg.norm(group_embs[a])
        if n > 0:
            group_embs[a] = group_embs[a] / n
        group_cnt[a] = total
        # 把所有 b 段映射到 a
        for i in group_segs.get(b, []):
            segments[i]["speaker"] = a
        # 删除 b
        del group_embs[b]
        del group_cnt[b]
        group_segs[a].extend(group_segs.get(b, []))
        del group_segs[b]

    # 最终重映射为连续 ID
    final_speakers = sorted(set(seg["speaker"] for seg in segments))
    final_map = {old: new for new, old in enumerate(final_speakers)}
    for seg in segments:
        seg["speaker"] = final_map[seg["speaker"]]

    return segments, len(final_speakers)


async def handle_diarize(request):
    """说话人分离端点
    接收音频文件，返回说话人时间段
    """
    if diarizer is None:
        return web.json_response({"error": "diarizer not loaded"}, status=503)

    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(
            executor, run_diarization, diarizer, tmp.name
        )
        return web.json_response(result)
    except Exception as e:
        print(f"[Diarizer] 分析失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_health(request):
    # 2026-09-04: 0.6B 精转+快速引擎，1.7B 可选 fallback
    if qwen3_asr_engine is None and not ASR_QUICK_ENGINES:
        return web.json_response(
            {"status": "error", "reason": "no asr engine loaded"}, status=503
        )
    engines = []
    if qwen3_asr_engine:
        engines.append("qwen3_asr_1.7b")
    if ASR_QUICK_ENGINES:
        engines.append("qwen3_asr_0.6b")
    return web.json_response(
        {"status": "ok", "engines": engines, "primary": "qwen3_asr_0.6b" if ASR_QUICK_ENGINES else "qwen3_asr_1.7b"}
    )


async def handle_engines(request):
    available = []
    for name in engines:
        available.append({"name": name, "available": True})
    # v3: 主引擎固定为 qwen3_asr
    return web.json_response({
        "engines": available,
        "primary": "qwen3_asr",
        "qwen3_loaded": qwen3_asr_engine is not None,
        "model": "qwen3_asr_1.7b",
    })


async def handle_get_hotwords(request):
    """热词查询端点（v3: 场景热词）

    返回指定场景的热词列表：
      GET /get_hotwords?scene=财务
      GET /get_hotwords            # 返回所有场景
    """
    scene = request.query.get("scene")
    if scene:
        words = HOTWORD_SCENES.get(scene, [])
        return web.json_response({"scene": scene, "hotwords": words, "count": len(words)})

    result = {}
    for s, ws in HOTWORD_SCENES.items():
        result[s] = ws
    return web.json_response({"scenes": result})


async def handle_detect_scene(request):
    """场景识别端点（v3: 第二层场景热词）

    接收 JSON: {"text": "转写文本前200字"}
    返回: {"scene": "财务", "matched_keywords": [...]}

    用关键词匹配实现（轻量、无LLM调用、<10ms）。
    """
    try:
        data = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)

    text = data.get("text", "")
    if not text:
        return web.json_response({"error": "no text provided"}, status=400)

    text_lower = text.lower()
    scores = {}
    matched = {}
    for scene, keywords in SCENE_KEYWORDS.items():
        score = 0
        hits = []
        for kw in keywords:
            if kw.lower() in text_lower:
                score += 1
                hits.append(kw)
        if score > 0:
            scores[scene] = score
            matched[scene] = hits

    if not scores:
        return web.json_response({"scene": "通用", "scene_keywords": []})

    best_scene = max(scores, key=scores.get)
    return web.json_response({
        "scene": best_scene,
        "scene_keywords": matched[best_scene],
        "all_scores": scores,
    })


async def handle_transcribe(request):
    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    # 读取 engine 参数（从 query string）
    engine_name = request.query.get("engine", "auto")

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    # 写入临时文件
    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()

        # 选择引擎
        if engine_name == "auto":
            # 优先级：firered_native > sensevoice > firered_aed > whisper
            if "firered_native" in engines:
                result = await loop.run_in_executor(
                    executor, transcribe_firered_native, engines["firered_native"], tmp.name
                )
            elif "sensevoice" in engines:
                result = await loop.run_in_executor(
                    executor, transcribe_sensevoice, engines["sensevoice"], tmp.name
                )
            elif "firered_aed" in engines:
                result = await loop.run_in_executor(
                    executor, transcribe_firered_aed, engines["firered_aed"], tmp.name
                )
            else:
                return web.json_response({"error": "no engine available"}, status=503)
        elif engine_name == "all":
            # 多引擎对比模式
            results = {}
            for name, recognizer in engines.items():
                try:
                    if name == "firered_native":
                        r = await loop.run_in_executor(
                            executor, transcribe_firered_native, recognizer, tmp.name
                        )
                    elif name == "sensevoice":
                        r = await loop.run_in_executor(
                            executor, transcribe_sensevoice, recognizer, tmp.name
                        )
                    elif name == "firered_aed":
                        r = await loop.run_in_executor(
                            executor, transcribe_firered_aed, recognizer, tmp.name
                        )
                    results[name] = r
                except Exception as e:
                    results[name] = {"error": str(e), "engine": name}

            # 也跑 whisper 如果可用
            whisper_result = await loop.run_in_executor(
                executor, transcribe_whisper_cpp, tmp.name
            )
            if whisper_result:
                results["whisper_cpp"] = whisper_result

            return web.json_response({"engines": results})
        elif engine_name == "dual":
            # v3: 双引擎模式已废弃，直接走 Qwen3-ASR 主引擎
            if qwen3_asr_engine is None:
                return web.json_response({"error": "qwen3_asr engine not loaded"}, status=503)
            result = await loop.run_in_executor(
                executor, transcribe_qwen3_asr, qwen3_asr_engine, tmp.name
            )
            return web.json_response(result)
        elif engine_name == "qwen3_asr":
            # Qwen3-ASR 引擎
            if qwen3_asr_engine is None:
                return web.json_response({"error": "qwen3_asr engine not loaded"}, status=503)
            context = request.query.get("context") or None
            result = await loop.run_in_executor(
                executor, transcribe_qwen3_asr, qwen3_asr_engine, tmp.name, context
            )
            return web.json_response(result)
        else:
            # 指定引擎
            if engine_name not in engines:
                return web.json_response(
                    {"error": f"engine '{engine_name}' not available", "available": list(engines.keys())},
                    status=400,
                )
            recognizer = engines[engine_name]
            if engine_name == "firered_native":
                result = await loop.run_in_executor(
                    executor, transcribe_firered_native, recognizer, tmp.name
                )
            elif engine_name == "sensevoice":
                result = await loop.run_in_executor(
                    executor, transcribe_sensevoice, recognizer, tmp.name
                )
            elif engine_name == "firered_aed":
                result = await loop.run_in_executor(
                    executor, transcribe_firered_aed, recognizer, tmp.name
                )
            else:
                return web.json_response({"error": f"unknown engine: {engine_name}"}, status=400)

        return web.json_response(result)

    except Exception as e:
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


# === P5 新端点 ===


async def handle_punctuate(request):
    """标点恢复端点 — 使用 ct-transformer"""
    if punct_model is None:
        return web.json_response({"error": "punctuation model not loaded"}, status=503)

    try:
        data = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)

    text = data.get("text", "")
    if not text:
        return web.json_response({"error": "no text provided"}, status=400)

    loop = asyncio.get_event_loop()
    result = await loop.run_in_executor(executor, punct_model.add_punctuation, text)
    # 标点恢复后过滤口语填充词
    result = filter_fillers(result)
    return web.json_response({"text": result})


async def handle_vad_transcribe(request):
    """VAD 智能分段转写端点"""
    if vad_config is None:
        return web.json_response({"error": "VAD model not loaded"}, status=503)

    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(executor, run_vad_transcribe_impl, tmp.name)
        return web.json_response({"segments": result})
    except Exception as e:
        print(f"[VAD Transcribe] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_medium_transcribe(request):
    """引擎2端点：VAD中粒度分段 + FireRed转写"""
    if vad_config is None:
        return web.json_response({"error": "VAD model not loaded"}, status=503)

    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(executor, run_medium_transcribe_impl, tmp.name)
        return web.json_response({"segments": result})
    except Exception as e:
        print(f"[Medium Transcribe] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_full_transcribe(request):
    """主转写端点：长段精转（Qwen3-ASR 单引擎）

    支持 query 参数 context 传入热词字符串（Qwen3-ASR context biasing）
    """
    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    # 读取 context 热词参数（query string）
    context = request.query.get("context") or None

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(executor, run_full_transcribe_impl, tmp.name, context)
        return web.json_response({"segments": result})
    except Exception as e:
        print(f"[Full Transcribe] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_final_transcribe(request):
    """P7: 全量二次精转端点（录音停止后调用）

    与 /full_transcribe 相同算法（VAD分段+长段Qwen3-ASR），但：
    1. 转写前先降噪（afftdn）
    2. 若系统负载过高会先卸载 Ollama 4B 模型让出 CPU
    3. 返回带 engine="qwen3_asr_final" 标记的段，前端/客户端用于覆盖准实时结果

    用法同 /full_transcribe：POST 音频文件 + ?context=热词
    """
    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    context = request.query.get("context") or None
    # 降噪强度参数：light/medium/strong，默认 medium
    denoise_level = request.query.get("denoise", "medium")

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        # 高负载时先让 Ollama 让出资源
        load = get_load()
        if load > LOAD_HIGH:
            print(f"[FinalTranscribe] 负载 {load:.1f} > {LOAD_HIGH}，先卸载 Ollama 模型", flush=True)
            ollama_unload_all()
        result = await loop.run_in_executor(
            executor, run_full_transcribe_impl, tmp.name, context
        )
        # 标记引擎为 final（区别于准实时段）
        for seg in result:
            seg["engine"] = "qwen3_asr_final"
        return web.json_response({"segments": result, "mode": "final"})
    except Exception as e:
        print(f"[Final Transcribe] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_qwen3_transcribe(request):
    """Qwen3-ASR 转写端点（主引擎）

    接收音频文件，返回 {"text": ..., "duration": ..., "engine": "qwen3_asr"}
    支持 query 参数 context 传入热词字符串
    P8: 准实时段转写用 0.6B 快速引擎（快）；精转走 /full_transcribe 和 /final_transcribe（1.7B）
    """
    # 优先快速引擎（0.6B 双实例），没有则用精转引擎（1.7B）
    if not ASR_QUICK_ENGINES and qwen3_asr_engine is None:
        return web.json_response({"error": "qwen3_asr engine not loaded"}, status=503)

    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    # 读取 context 热词参数
    context = request.query.get("context") or None

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        # v2.5.0: 实时段引擎切换——paraformer 优先，异常自动回退 0.6B 池（不丢段）
        # v2.5.2: fallback 结构化可见——回退不再是静默行为，engine/fallback/fallback_reason/duration_ms
        # 一并返回，Rust 落段级 metadata，前端可提示"部分片段使用备用引擎"（引擎退化不再被掩盖）
        t0 = time.time()
        fallback_used, fallback_reason = False, ""
        if PARAFORMER_RT is not None:
            try:
                result = await loop.run_in_executor(executor, transcribe_paraformer_realtime, tmp.name)
            except Exception as e:
                print(f"[RealtimeEngine] paraformer 转写异常，回退 0.6B 池: {e}")
                fallback_used, fallback_reason = True, str(e)[:200]
                result = await loop.run_in_executor(
                    executor, transcribe_qwen3_asr_quick_pool, tmp.name, context
                )
        else:
            result = await loop.run_in_executor(
                executor, transcribe_qwen3_asr_quick_pool, tmp.name, context
            )
        # 保留实际引擎标识（paraformer_rt / qwen3_asr_quick）
        result["engine"] = result.get("engine", "qwen3_asr_quick")
        # v2.5.2 结构化可见字段
        result["requested_engine"] = "paraformer" if PARAFORMER_RT is not None else "qwen3_asr_quick"
        result["fallback"] = fallback_used
        result["fallback_reason"] = fallback_reason
        result["duration_ms"] = int((time.time() - t0) * 1000)
        return web.json_response(result)
    except Exception as e:
        print(f"[Qwen3-ASR] 转写失败: {e}")
        import traceback
        traceback.print_exc()
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_cross_validate(request):
    """交叉校验端点（v3: 已停用）

    v2 验证结论：交叉校验是负优化（Qwen3直出75.6分 → 交叉校验后61.6分）。
    v3 起不再执行 LLM 选择题校验，直接返回引擎3（Qwen3-ASR）结果。
    保留端点兼容旧前端调用，但只透传 engine3 结果。
    """
    try:
        data = await request.json()
    except Exception:
        return web.json_response({"error": "invalid JSON"}, status=400)

    engine3 = data.get("engine3", [])
    # v3: 直接透传 engine3（Qwen3-ASR 直出），不再做交叉校验
    return web.json_response({"segments": engine3, "mode": "direct_qwen3"})


async def handle_enroll_speaker(request):
    """声纹注册端点"""
    if embedding_extractor is None or embedding_manager is None:
        return web.json_response({"error": "embedding model not loaded"}, status=503)

    reader = await request.multipart()

    name = None
    audio_data = None

    async for part in reader:
        if part.name == "name":
            name = (await part.read()).decode("utf-8")
        elif part.name == "file":
            audio_data = await part.read()

    if not name or not audio_data:
        return web.json_response({"error": "need name and file"}, status=400)

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        embedding = await loop.run_in_executor(
            executor, extract_embedding_impl, embedding_extractor, tmp.name
        )

        # 保存声纹
        np.save(os.path.join(VOICEPRINT_DIR, f"{name}.npy"), embedding)
        embedding_manager.add(name, embedding)

        return web.json_response({"success": True, "name": name, "embedding_size": len(embedding)})
    except Exception as e:
        print(f"[Enroll] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_identify_speaker(request):
    """声纹识别端点"""
    if embedding_extractor is None:
        return web.json_response({"error": "embedding model not loaded"}, status=503)

    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)

    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)

    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()

    try:
        loop = asyncio.get_event_loop()
        embedding = await loop.run_in_executor(
            executor, extract_embedding_impl, embedding_extractor, tmp.name
        )

        # 搜索最佳匹配
        threshold = 0.5
        best_match = None
        best_score = 0.0

        os.makedirs(VOICEPRINT_DIR, exist_ok=True)
        for f in os.listdir(VOICEPRINT_DIR):
            if f.endswith('.npy'):
                ref_name = f[:-4]
                ref_embedding = np.load(os.path.join(VOICEPRINT_DIR, f))
                score = float(np.dot(embedding, ref_embedding) / (np.linalg.norm(embedding) * np.linalg.norm(ref_embedding)))
                if score > best_score:
                    best_score = score
                    best_match = ref_name

        if best_match and best_score >= threshold:
            return web.json_response({"name": best_match, "score": round(best_score, 4), "identified": True})
        else:
            return web.json_response({"name": None, "score": round(best_score, 4), "identified": False})
    except Exception as e:
        print(f"[Identify] 失败: {e}")
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)


async def handle_list_voiceprints(request):
    """列出已注册的声纹"""
    os.makedirs(VOICEPRINT_DIR, exist_ok=True)
    voiceprints = []
    for f in os.listdir(VOICEPRINT_DIR):
        if f.endswith('.npy'):
            voiceprints.append(f[:-4])
    return web.json_response({"voiceprints": voiceprints})


async def handle_delete_voiceprint(request):
    """删除已注册的声纹"""
    data = await request.json()
    name = data.get("name", "")
    if not name:
        return web.json_response({"error": "no name provided"}, status=400)

    filepath = os.path.join(VOICEPRINT_DIR, f"{name}.npy")
    if os.path.exists(filepath):
        os.remove(filepath)
        if embedding_manager:
            embedding_manager.remove(name)
        return web.json_response({"success": True, "name": name})
    else:
        return web.json_response({"error": "voiceprint not found"}, status=404)


async def on_startup(app):
    print("[Sherpa ASR] 初始化引擎...", flush=True)

    # v4: 双引擎架构
    #   2026-09-04: 1.7B 纯 CPU RTF~5-10 不可接受，精转改用 0.6B
    #   1. 精转引擎：0.6B（VAD 长段精转 + 准实时段转写，RTF~0.27）
    #   2. 1.7B 保留 fallback（通过环境变量 BIJIAN_USE_17B=1 启用）
    print("[Sherpa ASR] 加载 Qwen3-ASR 引擎（0.6B 精转+快速）...", flush=True)
    t0 = time.time()
    global qwen3_asr_engine, qwen3_asr_engine_quick, ASR_QUICK_ENGINES
    
    if os.environ.get("BIJIAN_USE_17B"):
        # 可选：加载 1.7B 作为精转 fallback
        q3 = await asyncio.get_event_loop().run_in_executor(executor, init_qwen3_asr)
        if q3:
            qwen3_asr_engine = q3
        print(f"  1.7B 耗时 {time.time()-t0:.1f}s", flush=True)
    
    # 0.6B：精转 + 快速转写共用
    q3q = await asyncio.get_event_loop().run_in_executor(
        executor, init_qwen3_asr,
        QWEN3_ASR_MODEL_DIR_FALLBACK, "0.6B",
        2048, 40.0,  # 2026-09-04: chunk_size 改 40s（精转用长段，比 10s 准实时段质量更好）
    )
    if q3q:
        qwen3_asr_engine_quick = q3q
        ASR_QUICK_ENGINES.append((q3q, threading.Lock()))
    print(f"  0.6B 耗时 {time.time()-t0:.1f}s", flush=True)

    # 2026-09-23: 恢复双实例负载均衡（2026-09-03 设计，9-04 改 chunk 时被覆盖丢失）。
    # 单实例串行导致 10s 准实时段积压（单段 ~10s 跟不上录音节奏）。
    # 每个实例独立持锁，llama context 非线程安全不可共享。
    t1 = time.time()
    q3q2 = await asyncio.get_event_loop().run_in_executor(
        executor, init_qwen3_asr,
        QWEN3_ASR_MODEL_DIR_FALLBACK, "0.6B",
        2048, 40.0,
    )
    if q3q2:
        ASR_QUICK_ENGINES.append((q3q2, threading.Lock()))
        print(f"  0.6B 第二实例 耗时 {time.time()-t1:.1f}s", flush=True)
    else:
        print("[Sherpa ASR] 第二实例加载失败，回退单实例（不致命）", flush=True)

    print(f"[Sherpa ASR] 引擎: 1.7B={'ON' if qwen3_asr_engine else 'OFF'} | "
          f"0.6B 精转+快速: {len(ASR_QUICK_ENGINES)} 实例", flush=True)

    # 4. 说话人分离模型
    print("[Sherpa ASR] 加载说话人分离模型...", flush=True)
    t0 = time.time()
    global diarizer
    dia = await asyncio.get_event_loop().run_in_executor(executor, init_diarizer)
    if dia:
        diarizer = dia
    print(f"  耗时 {time.time()-t0:.1f}s", flush=True)

    # 5. P5: Silero VAD
    print("[Sherpa ASR] 加载 Silero VAD...", flush=True)
    t0 = time.time()
    await asyncio.get_event_loop().run_in_executor(executor, init_vad)
    print(f"  耗时 {time.time()-t0:.1f}s", flush=True)

    # 6. P5: ct-transformer 标点恢复
    print("[Sherpa ASR] 加载 ct-transformer 标点恢复...", flush=True)
    t0 = time.time()
    global punct_model
    punct = await asyncio.get_event_loop().run_in_executor(executor, init_punctuation)
    if punct:
        punct_model = punct
    print(f"  耗时 {time.time()-t0:.1f}s", flush=True)

    # 7. P5: 声纹提取器
    print("[Sherpa ASR] 加载声纹提取器...", flush=True)
    t0 = time.time()
    global embedding_extractor, embedding_manager
    ext, mgr = await asyncio.get_event_loop().run_in_executor(executor, init_embedding_extractor)
    if ext:
        embedding_extractor = ext
        embedding_manager = mgr
    print(f"  耗时 {time.time()-t0:.1f}s", flush=True)

    # 8. v2.5.0（9-25）: 实时段引擎切换——paraformer 加载（开关见模块顶部说明）
    global PARAFORMER_RT
    if REALTIME_ENGINE == "paraformer":
        print("[Sherpa ASR] 实时引擎切换 → paraformer (BIJIAN_REALTIME_ENGINE=paraformer)", flush=True)
        prt = await asyncio.get_event_loop().run_in_executor(executor, init_paraformer)
        if prt:
            PARAFORMER_RT = prt
            print("[Sherpa ASR] Paraformer RT 加载成功（10s 段预计 ~0.3s）", flush=True)
        else:
            print("[Sherpa ASR] Paraformer RT 加载失败，实时段回退 0.6B 池", flush=True)


async def on_cleanup(app):
    executor.shutdown(wait=False)


# 2026-09-03: 50MB 限制导致 >50 分钟录音（~105MB full_audio.wav）的精转/说话人分离被 413 拒绝
# 改为 2GB，覆盖 ~17 小时录音（16kHz/16bit/mono 约 2.9MB/分钟）
app = web.Application(client_max_size=2 * 1024 * 1024 * 1024)
app.router.add_get("/health", handle_health)
app.router.add_get("/engines", handle_engines)
app.router.add_get("/get_hotwords", handle_get_hotwords)
app.router.add_post("/detect_scene", handle_detect_scene)
app.router.add_post("/transcribe", handle_transcribe)
app.router.add_post("/diarize", handle_diarize)
# P5 新端点
app.router.add_post("/punctuate", handle_punctuate)
app.router.add_post("/vad_transcribe", handle_vad_transcribe)
app.router.add_post("/medium_transcribe", handle_medium_transcribe)
app.router.add_post("/full_transcribe", handle_full_transcribe)
app.router.add_post("/final_transcribe", handle_final_transcribe)
app.router.add_post("/cross_validate", handle_cross_validate)
app.router.add_post("/qwen3_transcribe", handle_qwen3_transcribe)
app.router.add_post("/enroll_speaker", handle_enroll_speaker)
app.router.add_post("/identify_speaker", handle_identify_speaker)
app.router.add_get("/voiceprints", handle_list_voiceprints)
app.router.add_post("/voiceprints/delete", handle_delete_voiceprint)
app.on_startup.append(on_startup)
app.on_cleanup.append(on_cleanup)

if __name__ == "__main__":
    print("[Sherpa ASR] 启动 HTTP 服务 :8083")
    web.run_app(app, host="127.0.0.1", port=8083)

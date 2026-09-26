#!/usr/bin/env python3
"""
FireRed ASR HTTP Server
提供 /health 和 /transcribe 两个端点，供笔尖 asr_client.py 调用
"""
import os
import sys
import json
import tempfile
import wave
import time
import concurrent.futures
from aiohttp import web

# 导入 FireRed 推理引擎（路径通过环境变量配置）
# v2.3.3: 默认路径从 Trae 工作目录迁移到 ~/Library/Application Support/PanNote/models/firered_lib
#         Trae 工作目录随时可能被清理，应用私有目录稳定可靠
FIRERED_LIB_DIR = os.environ.get(
    "FIRERED_LIB_DIR",
    os.path.expanduser("~/Library/Application Support/PanNote/models/firered_lib"),
)
sys.path.insert(0, FIRERED_LIB_DIR)
from firered_asr import FireRedASR

# 全局 ASR 引擎（启动时加载）
asr_engine = None
# 线程池用于执行阻塞推理（encoder 1.2GB，推理较慢）
executor = concurrent.futures.ThreadPoolExecutor(max_workers=1)

async def handle_health(request):
    """健康检查"""
    if asr_engine is None:
        return web.json_response({"status": "error", "reason": "engine not loaded"}, status=503)
    return web.json_response({"status": "ok", "model": "FireRedASR2"})

async def handle_transcribe(request):
    """转写音频"""
    reader = await request.multipart()
    field = await reader.next()
    if field is None:
        return web.json_response({"error": "no file uploaded"}, status=400)
    
    # 读取上传的音频数据
    audio_data = await field.read()
    if not audio_data:
        return web.json_response({"error": "empty file"}, status=400)
    
    # 写入临时 WAV 文件
    tmp = tempfile.NamedTemporaryFile(suffix=".wav", delete=False)
    tmp.write(audio_data)
    tmp.close()
    
    try:
        # 在线程池中执行阻塞推理
        loop = asyncio.get_event_loop()
        result = await loop.run_in_executor(executor, asr_engine.transcribe, tmp.name)
        
        # 转换为 asr_client 期望的格式
        response = {
            "text": result["text"],
            "duration": result["duration"],
            "segments": [{
                "start": 0,
                "end": result["duration"],
                "text": result["text"],
                "confidence": 0.9
            }]
        }
        return web.json_response(response)
    except Exception as e:
        return web.json_response({"error": str(e)}, status=500)
    finally:
        os.unlink(tmp.name)

async def on_startup(app):
    global asr_engine
    print("[FireRed ASR] 正在加载模型...")
    t0 = time.time()
    asr_engine = FireRedASR()
    print(f"[FireRed ASR] 模型加载完成，耗时 {time.time()-t0:.2f}s")

async def on_cleanup(app):
    executor.shutdown(wait=False)

import asyncio
app = web.Application(client_max_size=50*1024*1024)  # 最大 50MB 上传
app.router.add_get("/health", handle_health)
app.router.add_post("/transcribe", handle_transcribe)
app.on_startup.append(on_startup)
app.on_cleanup.append(on_cleanup)

if __name__ == "__main__":
    print("[FireRed ASR] 启动 HTTP 服务 :8080")
    web.run_app(app, host="127.0.0.1", port=8082)

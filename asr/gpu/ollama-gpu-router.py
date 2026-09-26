#!/usr/bin/env python3
"""
ollama-gpu-router.py v3 — PanNote 专用 Ollama API 智能路由代理
================================================================
v3 核心改进：
  1. 长调用也先试 GPU：按 token 估算决定路由，而非简单字数切
     - prompt < ~1200 字符（约 800 tokens）→ GPU（留 2048-800=1248 给生成）
     - prompt > ~1200 字符 → Ollama（保大上下文）
  2. GPU 转发时透传 format=json（PanNote reduce 步骤需要 JSON 输出）
  3. Ollama 兜底时原样透传完整 payload（含 format/options/num_ctx）
  4. 全程热自适应节流（v2 保留）+ GPU 健康检查自动降级/恢复

使用：
  1. 启动 GPU 服务：双击 qwen3-4b-GPU启动.command（8081）
  2. 启动本代理：python3 ollama-gpu-router.py（监听 11435）
  3. PanNote：OLLAMA_URL=http://127.0.0.1:11435
  4. 回滚：OLLAMA_URL 改回 http://127.0.0.1:11434
"""
import json
import math
import subprocess
import time
import threading
import urllib.request
import urllib.error
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

GPU_SERVER = "http://127.0.0.1:8081"
OLLAMA = "http://127.0.0.1:11434"
ROUTER_PORT = 11435

# ---- 路由阈值 ----
# GPU ctx=2048，一级摘要 prompt 约 2300 字（≈1530 tokens）+ 280 tokens 生成 = 1810 < 2048 ✅
# 四级压缩方案：一/二/三级 prompt 均在 2300 字以内，全走 GPU
PROMPT_CHAR_LIMIT = 2400   # 低于此值走 GPU，高于走 Ollama
GPU_MAX_TOKENS = 1200      # GPU 侧生成上限（留余量给 ctx=2048）

# ---- v6 穿测数据驱动：num_predict 感知路由 ----
# 实测：GPU prompt eval 37 tps < CPU 64 tps（MoltenVK 转换开销）
#        GPU generation 30 tps >> CPU 8.4 tps
# 所以：短生成+长输入 = prompt密集型 → CPU 更快
#       长生成 = gen密集型 → GPU 碾压
# 阈值：num_predict ≤ 500 且输入 ≥ 500 字符 → CPU（块压缩场景）
#       否则走 GPU（最终组装 gen=6000、短问答 gen=1000+ 等）
CPU_BETTER_NP = 500        # 生成 token 少于此值 + 输入足够长 → CPU 占优
CPU_BETTER_MIN_CHARS = 500  # 输入须超此字符数才算 prompt 密集型

# ---- v6 热债驱动自动 CPU 散热 ----
# 热债超阈值时主动走 CPU（让 GPU 散热，不白等歇息）
HEAT_DEBT_CPU_THRESHOLD = 400.0  # 热债 ≥此值 → 切 CPU 散热（实测 VRM 超此 gen 降到 15 tps）

# ---- 热自适应节流 v4：动态参数（歇息/窗口随限速深度浮动）----
# 用户敲定基准循环：60s工作+30s歇（实测限速稳定42-48%，吞吐≈20tok/s=2x CPU）
# v4 把它做成自适应：热了自动缩窗加歇，冷了自动放开窗口
HEALTH_CHECK_INTERVAL = 60
THERMAL_CHECK_MIN = 40      # 歇后仍低于此值才降级 Ollama

def cooldown_for(limit):
    """歇息时长：限速越深歇越久"""
    if limit < 30: return 40   # 深度热谷（22-28%实测区间）：歇 40s
    if limit < 40: return 30   # 中度热谷：歇 30s（基准循环的歇）
    if limit < 60: return 15   # 轻度受限：歇 15s
    return 0                   # ≥60% 健康：不歇

def burst_window(limit):
    """工作窗口：限速越好跑越久"""
    if limit >= 80: return 60   # 健康：连续 60s（基准循环的工作）
    if limit >= 40: return 45   # 中速：45s
    return 30                   # 热谷：短窗 30s

# ---- 全局状态 ----
gpu_healthy = True
gpu_healthy_lock = threading.Lock()
gpu_active_seconds = 0.0
gpu_active_lock = threading.Lock()
last_gpu_request_end = 0.0

# ---- 热债务累积器 v5（2026-09-20 固化，用户拍板的数学模型）----
# 物理模型：VRM 慢分量热累积，τ_slow ≈ 300s（实测：短突发+间歇限速可回升 100%，
#           连续满载 2-3 分钟掉 22-33%，深谷恢复需 3-5 分钟轻载）
# 离散化：  每次GPU工作 dt → heat_debt += dt；每次休息 dr → heat_debt *= e^(-dr/τ)
# 冷却函数：cooldown = cooldown_for(limit) + K_DEBT × heat_debt
# 深歇触发：累计GPU工作(自上次深歇) ≥ DEEP_INTERVAL → 强制深歇 DEEP_REST 秒
TAU_SLOW = 300.0          # VRM 慢时间常数（秒）
K_DEBT = 0.05             # 热债务→额外冷却的线性系数（每20s净热债多歇1s）
DEEP_INTERVAL = 600.0     # 每累计600s GPU工作触发深歇
DEEP_REST = 120           # 深歇时长（秒）

heat_debt = 0.0           # 当前热债务（秒×归一化）
gpu_work_since_deep = 0.0  # 自上次深歇以来的累计 GPU 工作秒数
heat_lock = threading.Lock()

def add_heat(dt):
    """GPU 完成 dt 秒工作：累积热债务"""
    global heat_debt, gpu_work_since_deep
    with heat_lock:
        heat_debt += dt
        gpu_work_since_deep += dt

def cool_heat(dr):
    """歇息 dr 秒：热债务指数衰减"""
    global heat_debt
    with heat_lock:
        heat_debt *= math.exp(-dr / TAU_SLOW)

def deep_rest_reset():
    """深歇完成：清零累计工作，热债务大幅衰减"""
    global heat_debt, gpu_work_since_deep
    with heat_lock:
        heat_debt *= math.exp(-DEEP_REST / TAU_SLOW)
        gpu_work_since_deep = 0.0

def debt_bonus():
    """热债务贡献的额外冷却秒数"""
    with heat_lock:
        return int(heat_debt * K_DEBT)

def get_thermal_limit():
    try:
        out = subprocess.check_output(["pmset", "-g", "therm"], timeout=5).decode()
        for line in out.splitlines():
            if "CPU_Speed_Limit" in line:
                return int(line.split()[-1])
    except Exception:
        pass
    return 100

def check_gpu_health():
    try:
        req = urllib.request.Request(GPU_SERVER + "/health", method="GET")
        with urllib.request.urlopen(req, timeout=5) as r:
            return r.status == 200
    except Exception:
        return False

def health_monitor():
    global gpu_healthy
    while True:
        time.sleep(HEALTH_CHECK_INTERVAL)
        ok = check_gpu_health()
        with gpu_healthy_lock:
            old = gpu_healthy
            gpu_healthy = ok
        if old != ok:
            if ok:
                print(f"[router] GPU server 恢复健康，切回 GPU 路由")
            else:
                print(f"[router] GPU server 不可用，后续请求转 Ollama")

def http_post(url, payload, timeout=600):
    body = json.dumps(payload).encode()
    req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.load(r)

def http_pass(req):
    length = int(req.headers.get("Content-Length", 0) or 0)
    data = req.rfile.read(length) if length else None
    url = OLLAMA + req.path
    r = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"},
                               method=req.command)
    try:
        with urllib.request.urlopen(r, timeout=600) as resp:
            return resp.status, resp.read()
    except urllib.error.HTTPError as e:
        return e.code, e.read()

def route_to_ollama(payload, label, handler):
    """Ollama 兜底：原样透传完整 payload（含 format/options/num_ctx）"""
    t0 = time.time()
    try:
        resp = http_post(OLLAMA + "/api/chat", payload)
        dt = time.time() - t0
        ec = resp.get("eval_count", 0)
        tps = ec / dt if dt > 0 and ec else 0
        print(f"[router] {label} -> Ollama CPU, {dt:.1f}s ({ec}tok, {tps:.1f}tps)")
        handler._send(200, json.dumps(resp).encode())
    except Exception as e:
        print(f"[router] {label} -> Ollama 失败: {e}")
        handler._send(502, json.dumps({"error": str(e)}).encode())

class Router(BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        pass

    def _send(self, code, body_bytes):
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.end_headers()
        self.wfile.write(body_bytes)

    def do_GET(self):
        code, body = http_pass(self)
        self._send(code, body)

    def do_POST(self):
        if self.path != "/api/chat":
            code, body = http_pass(self)
            self._send(code, body)
            return

        length = int(self.headers.get("Content-Length", 0) or 0)
        try:
            payload = json.loads(self.rfile.read(length))
        except json.JSONDecodeError:
            self._send(400, json.dumps({"error": "invalid JSON"}).encode())
            return

        options = payload.get("options") or {}
        num_predict = options.get("num_predict", 1024)
        messages = payload.get("messages") or []
        content = " ".join(m.get("content", "") for m in messages)
        n_chars = len(content)
        fmt = payload.get("format")  # PanNote reduce 步骤传 "json"

        # ---- v6 smart routing：num_predict 感知 + 热债自动散热 ----
        # 规则1：长输入 → Ollama（超 GPU ctx）
        if n_chars > PROMPT_CHAR_LIMIT:
            route_to_ollama(payload, f"长({n_chars}字符)", self)
            return

        # 规则2：prompt密集型（短生成+长输入）→ CPU 更快
        # 穿测数据：CPU prompt eval 64 tps vs GPU 37 tps
        if num_predict <= CPU_BETTER_NP and n_chars >= CPU_BETTER_MIN_CHARS:
            route_to_ollama(payload, f"prompt密集(np={num_predict},{n_chars}字符)->CPU", self)
            return

        # 规则3：热债过高 → 主动走 CPU 散热（不白等歇息）
        with heat_lock:
            current_debt = heat_debt
        if current_debt >= HEAT_DEBT_CPU_THRESHOLD:
            route_to_ollama(payload, f"热债散热({current_debt:.0f}s)->CPU", self)
            return

        # ---- 短调用：检查 GPU 可用性 ----
        global gpu_healthy, gpu_active_seconds, last_gpu_request_end
        with gpu_healthy_lock:
            healthy = gpu_healthy
        if not healthy:
            route_to_ollama(payload, f"短({n_chars}字符,GPU不可用)", self)
            return

        # ---- 热自适应节流 v4：动态调节 ----
        limit = get_thermal_limit()
        cd = cooldown_for(limit)
        if cd > 0:
            print(f"[router] 热状态(限速{limit}%)，自适应歇{cd}s...")
            time.sleep(cd)
            limit = get_thermal_limit()
            cd2 = cooldown_for(limit)
            if cd2 > 0:
                print(f"[router] 一轮歇后仍{limit}%，再歇{cd2}s...")
                time.sleep(cd2)
                limit = get_thermal_limit()
                if limit < THERMAL_CHECK_MIN:
                    route_to_ollama(payload, f"短({n_chars}字符,两轮散热后仍{limit}%)", self)
                    return

        # ---- v5 深歇触发：累计GPU工作达阈值 → 强制长歇散热 ----
        with heat_lock:
            since_deep = gpu_work_since_deep
        if since_deep >= DEEP_INTERVAL:
            print(f"[router-v5] 累计GPU工作{since_deep:.0f}s≥{DEEP_INTERVAL:.0f}s，热债{heat_debt:.0f}s，强制深歇{DEEP_REST}s...")
            time.sleep(DEEP_REST)
            deep_rest_reset()
            limit = get_thermal_limit()
            print(f"[router-v5] 深歇后限速{limit}%，热债余{heat_debt:.0f}s")
            if limit < THERMAL_CHECK_MIN:
                route_to_ollama(payload, f"短({n_chars}字符,深歇后仍{limit}%)", self)
                return

        win = burst_window(limit)
        with gpu_active_lock:
            burst_used = gpu_active_seconds
        if burst_used >= win:
            cd = cooldown_for(get_thermal_limit()) or 20
            bonus = debt_bonus()
            total_cd = cd + bonus
            print(f"[router] 动态窗口({win}s)用满({burst_used:.0f}s)，冷却{total_cd}s(基座{cd}+热债{bonus})...")
            time.sleep(total_cd)
            cool_heat(total_cd)
            with gpu_active_lock:
                gpu_active_seconds = 0.0
            limit = get_thermal_limit()
            if limit < THERMAL_CHECK_MIN:
                route_to_ollama(payload, f"短({n_chars}字符,冷却后仍{limit}%)", self)
                return

        now = time.time()
        gap = now - last_gpu_request_end
        if 0 < gap < 3:
            time.sleep(3 - gap)

        # ---- 转发 GPU ----
        # 构造 OpenAI 格式，透传 format=json（llama-server 支持 response_format）
        openai_payload = {
            "messages": messages,
            "max_tokens": min(num_predict, GPU_MAX_TOKENS),
        }
        if fmt == "json":
            openai_payload["response_format"] = {"type": "json_object"}

        t0 = time.time()
        try:
            resp = http_post(GPU_SERVER + "/v1/chat/completions", openai_payload, timeout=120)
            dt = time.time() - t0
            text = resp["choices"][0]["message"]["content"]
            usage = resp.get("usage", {})
            ct = usage.get("completion_tokens", 0)

            with gpu_active_lock:
                gpu_active_seconds += dt
            add_heat(dt)  # v5：累积热债务
            last_gpu_request_end = time.time()

            tps = ct / dt if dt > 0 and ct > 0 else 0
            limit = get_thermal_limit()
            tag = f"短({n_chars}字符,json)" if fmt == "json" else f"短({n_chars}字符)"
            print(f"[router] {tag} -> GPU, {dt:.1f}s "
                  f"(出{ct}tok, {tps:.1f}tps, 限速{limit}%)")

            ollama_resp = {
                "model": payload.get("model", "qwen3-4b-32k"),
                "created_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
                "message": {"role": "assistant", "content": text},
                "done": True,
                "done_reason": "stop",
                "eval_count": ct,
            }
            self._send(200, json.dumps(ollama_resp).encode())
        except urllib.error.URLError:
            with gpu_healthy_lock:
                gpu_healthy = False
            print(f"[router] GPU不可达，标记不健康，转 Ollama")
            route_to_ollama(payload, f"短({n_chars}字符,GPU不可达)", self)
        except Exception as e:
            print(f"[router] GPU异常: {e}，转 Ollama")
            route_to_ollama(payload, f"短({n_chars}字符,GPU异常)", self)

if __name__ == "__main__":
    print(f"[router-v3] 监听 http://127.0.0.1:{ROUTER_PORT}")
    print(f"[router-v6] 短(≤{PROMPT_CHAR_LIMIT}字符)->GPU | 长->Ollama")
    print(f"[router-v6] smart routing: prompt密集(np≤{CPU_BETTER_NP}+输入≥{CPU_BETTER_MIN_CHARS}字符)->CPU | 热债≥{HEAT_DEBT_CPU_THRESHOLD:.0f}s->CPU散热")
    print(f"[router-v6] format=json 透传 | options 原样兑底")
    print(f"[router-v6] 动态热调节: 歇=40/30/15s@限速<30/<40/<60% | 窗=60/45/30s@≥80/40-80/<40%")
    print(f"[router-v6] 热债务累积器: τ={TAU_SLOW}s K={K_DEBT} | 深歇{DEEP_REST}s@累计{DEEP_INTERVAL:.0f}s")
    print(f"[router-v6] GPU探活: 每{HEALTH_CHECK_INTERVAL}s，自动降级/恢复")

    t = threading.Thread(target=health_monitor, daemon=True)
    t.start()

    ThreadingHTTPServer(("127.0.0.1", ROUTER_PORT), Router).serve_forever()

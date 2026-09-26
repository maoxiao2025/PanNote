---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: '8dcdc632-16ec-4480-9ed2-db76e6e3e3fc'
  PropagateID: '8dcdc632-16ec-4480-9ed2-db76e6e3e3fc'
  ReservedCode1: '77f5d370-c035-44ce-9e11-d8f098cf7eb1'
  ReservedCode2: '77f5d370-c035-44ce-9e11-d8f098cf7eb1'
---

# PanNote 新机器迁移指南（v2.6.0）

> 配套 `scripts/install_pannote.sh`。目标：新 Mac 从零到能录一场会议 ≤30 分钟。
> 前提：新机为 macOS（Apple Silicon 或 Intel 均可，CPU 转写无 GPU 依赖）。

## 迁移清单（从旧机拷到新机）

| # | 内容 | 旧机路径 | 新机路径 | 方式 |
|---|---|---|---|---|
| 1 | 源码 | ~/Documents/PanNote | 同路径 | rsync（含 .git，保留版本史） |
| 2 | Python venv | ~/.workbuddy/binaries/python/envs/bijian | 同路径 | rsync（含依赖，省重建） |
| 3 | 模型（大文件） | ~/Documents/PanNote/models | 同路径 | rsync（0.6B+Paraformer+标点+VAD，共数 GB，局域网 rsync 最快） |
| 4 | launchd plist | ~/Library/LaunchAgents/com.bijian.asr.agent.plist | 同路径 | scp |
| 5 | 业务数据（可选） | ~/Library/Application Support/com.bijian.app | 同路径 | rsync（history.db + audio_cache；不想带历史则跳过，首启自动建库） |
| 6 | Rust 工具链 | — | rustup.rs 安装 | 新机安装 |
| 7 | Node + 依赖 | — | brew install node；npm install | 新机安装 |

## 执行顺序（新机）

```bash
# 1. 拷贝完成后，构建（约 10-20 分钟）
cd ~/Documents/PanNote && cargo tauri build

# 2. 安装 App
cp -r target/release/bundle/macos/PanNote.app /Applications/

# 3. 注册 ASR 服务
launchctl load ~/Library/LaunchAgents/com.bijian.asr.agent.plist

# 4. 自检（6 步全绿即可用）
bash scripts/install_pannote.sh
```

## 已知边界（诚实声明）

- Python venv 依赖私有目录（~/.workbuddy），rsync 前后路径必须一致，否则 `asr_wrapper.sh` 找不到解释器。
- 首次启动 App 需在「系统设置 → 隐私与安全性 → 麦克风」授权（v2.5.0 起授权被拒会 20 秒内强提醒，不再静默全零）。
- v2.6.0 起新录音统一写入 com.bijian.app/audio_cache；旧机三套历史目录（PanNote/笔尖）如已 rsync 迁移，读取回链不受影响（session_dir 存绝对路径）。
- 本指南在新机器上的完整实测尚未执行（无第二台机器），首次真实迁移时按实际情况修订本文件。

> AI生成
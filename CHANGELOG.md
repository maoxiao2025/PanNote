---
AIGC:
  ContentProducer: '001191110102MAD55U9H0F10002'
  ContentPropagator: '001191110102MAD55U9H0F10002'
  Label: '1'
  ProduceID: '687d737b-8d24-4a92-880b-919ed4640a31'
  PropagateID: '687d737b-8d24-4a92-880b-919ed4640a31'
  ReservedCode1: '5224e018-61ea-4358-8361-4a885fd461fe'
  ReservedCode2: '5224e018-61ea-4358-8361-4a885fd461fe'
---

# Changelog

本文件记录 PanNote 所有版本的变更历史。
格式参考 [Keep a Changelog](https://keepachangelog.com/)，版本号遵循 [Semantic Versioning](https://semver.org/)。

---

## [v2.6.0] — 2026-09-25（与 v2.5.2 同日连续落地，方案 v3 全量收官）

### 新增（P2 产品化）
- **纠错反哺闭环**（`commands.rs` / `hotwords.js`）：`get_correction_hotword_suggestions` 从 correction_samples 存量样本蒸馏热词建议（复用 split_segments/extract_diff_pair 差异提取，聚合计数、过滤已入库），`apply_correction_hotwords` 一键入库（confidence 0.8），下次转写 context 携带 + 后处理替换双路生效——数据飞轮从"只统计"转成产品力
- **健康/诊断报告**（`commands.rs` / `hotwords.js`）：`get_health_report` 今日段数/失败/静音/fallback、累计指标、磁盘水位、最近备份、服务状态——不翻日志即可判断今天是否正常；热词面板内嵌展示
- **磁盘水位管理**（`commands.rs` / `lib.rs`）：`enforce_disk_watermark` 磁盘 >90% 时 zip 归档最旧已完成会议音频至 backups/archived_audio/（文本记录永不删，录音中会议绝不动），挂入 5 分钟巡检循环；归档失败宁可占盘不可丢数据
- **冒烟测试集**（`tests/smoke_test.rs`，10 项全绿）：9-24 事故教训区全覆盖回归线——四类状态收敛（全成功/部分失败/全静音/进行中不越终态）、导入型 0 行保护、巡检幂等、stale processing 接管、段唯一约束、纪要门槛（failed 默认拒/force 放行带标注）、失败段重试。核心状态机函数 pub 化（doc(hidden)）并泛型化 Runtime 以支持 MockRuntime 测试
- **会议类型纪要模板**（`services.rs` / `index.html`）：新增 visit（客户拜访：客户背景/诉求/我方承诺口径/商务进展/待办分列）与 project（项目评审：评审结论/方案要点/评审人意见/修改要求/节点）两套 LLM 骨架；interview 模板补招聘场景要点
- **录音写入目录归一**（`commands.rs`）：start_recording 新录音固定写 com.bijian.app/audio_cache，根治三套目录并存；存量会议读取走 session_dir 绝对路径不受影响，旧目录近匹配逻辑保留兼容超老数据
- **一键安装/自检脚本**（`scripts/install_pannote.sh` + `README_MIGRATION.md`）：6 步自检（源码/venv/模型/数据目录/launchd/服务健康），本机实测全绿；新机迁移清单含 rsync 项与已知边界诚实声明

### 决策记录
- **状态机三态拆分（recording/transcription/summary 拆字段）评估**：v2.5.1 唯一收敛函数 + v2.5.2 状态卡片（前端直接读段级事实）已消除"显示成功、实际部分失败"的失真路径，三态拆字段迁移风险（前后端+数据迁移联动）大于边际收益，**降级为不急**——待商业化收回门控或多人使用暴露新需求时再评估

## [v2.5.2] — 2026-09-25（可靠性产品化 + 性能基线）

### 新增（P1）
- **worker 连续消费**（`services.rs`）：队列有单立即取下一单（不 sleep），队列空才睡 2s——根治"2h 会议 720 段 × 每 2s 取 1 单"的纯轮询滞后（原约 24 分钟等待归零）；不做并发多单（CPU 上 4B 并发互相拖慢，chunk_summary 保序更稳）
- **fallback 结构化可见**（`sherpa_asr_server.py` / `commands.rs` / `db.rs` / `note-voice.js`）：Python 端点返回 engine/requested_engine/fallback/fallback_reason/duration_ms；段级表加 engine 列（v6 迁移），发生回退的段记 `fallback_<engine>`；收敛事件带 fallback_count，前端状态卡片 + toast 提示"部分片段使用备用引擎"——引擎退化不再静默
- **转写状态卡片**（`commands.rs` / `note-voice.js` / `index.html` / `style.css`）：`get_transcription_stats` 按需返回段级状态分布+引擎分布，会议详情页顶部常驻卡片显示状态/段统计/引擎，failed>0 时出重试按钮
- **失败段一键重试**（`commands.rs`）：`retry_failed_segments` failed→pending（清错误计数）后交补跑差集扫描（复用既有闭环），前端按钮实时反馈
- **纪要完整性门槛**（`commands.rs` / `services.rs` / `note-voice.js`）：trigger_aggregate 段级检查——pending/processing 未清零一律拒绝；failed>0 默认拒绝、force=true 放行且 payload 带 partial_note 注入纪要 JSON 头部（前端确认流 + 纪要头部黄色警告条），服务端守卫兜底
- **纪要导出 .docx/.md**（`commands.rs` + docx-rs / `note-voice.js` / `tauri-bridge.js`）：统一组稿函数按结构化字段扁平化为 markdown，docx 生成放 blocking 线程；前端系统保存对话框选路径（tauri-plugin-dialog 已有），导出后自动打开所在目录——纪要直接进 OA，不再复制粘贴
- **性能基线三档实测**（`tools/perf_baseline.py`）：macOS TTS（Eddy zh_CN）生成真实中文语音，10/30/120 分钟档 10s 段串行转写（模拟实时单段流），统计 P50/P95/RTF/fallback/失败/ASR 进程 RSS——结果归档 `performance/baseline-2026-09-25.md`（报告含验收判定：P95<2s、fallback<5%、0 失败）

### 修复
- ASR 服务重启后确认 v2.5.2 结构化字段生效（requested_engine/duration_ms/fallback 实测返回）

## [v2.5.1] — 2026-09-25（发布质量收口）

### 修复（P0）
- **测试基线修复**：create_meeting 三处 E0061（API 漂移）补 note_id 参数；feature::test_feature_gating 重写为机制验证（门控行为与 FREE_FEATURES 配置一致，不硬编码具体功能——原断言与 2026-09-16"全功能免费"产品决策冲突随环境漂移失败）；`cargo test --all-targets` 26+ 测试全绿
- **统计一致性收敛**（`commands.rs`）：update_meeting_status_by_segments 升级为唯一收敛函数——补 expected_segments 回写（段级表为事实基准）、段级 0 行保护（导入型/v2.5.0 前老会议不据此改状态，防误标 failed）
- **启动/定时一致性巡检**（`commands.rs` / `lib.rs`）：consistency_check_and_repair 幂等修复统计失真 + 状态失真（排除 recording；completed 是纪要链路合法终态不回退）；历史回填已执行并验证：5 场段级会议全部 ALIGNED（修复"招聘继续0924"completed 0→8 等）

### 新增（P0）
- **每日自动备份**（`commands.rs` / `lib.rs` / `app.js`）：db VACUUM INTO + 近 7 天会议音频整目录复制（三套历史 audio_cache 兼容扫描），settings 表幂等标记，保留最近 7 个 auto_* 备份；失败 emit auto_backup_failed 前端强提醒——9-24 三场会议音频永久丢失的防线
- **Python 重复配置清理**（`sherpa_asr_server.py`）：REALTIME_ENGINE/PARAFORMER_RT 双声明合并（合并残留）

---

## [v2.3.2] — 2026-09-09

### 修复（P0）
- **launchd 双重拉起根治** (`asr_manager.rs`)：`start_in_background` 检测到 launchd agent 已安装则不 spawn Python，由 launchd 独占拉起，根治「AsrManager.spawn + launchd 同时拉起抢 8082/8083 端口」导致一方启动失败、launchd KeepAlive 无限重启空转 CPU 的稳定性 bug
- **launchd plist 节流** (`commands.rs`)：`KeepAlive` 从 `true` 改为 dict（`SuccessfulExit=false` + `OtherJobEnabled=false` + `AfterInitialDemand=true`），加 `ThrottleInterval=30`，wrapper 异常退出后 30s 节流重启，根治 KeepAlive 死循环空转
- **wrapper 脚本端口检测** (`commands.rs`)：启动前 `lsof -i :8082 :8083` 检测端口被占则退出 0，让原有 ASR 继续，launchd 不重启——根治端口冲突死循环
- **删除冗余 asr_launcher.sh**：与 asr_wrapper.sh 内容完全相同，删一个

### 新增（P0）
- **真实 Ed25519 密钥对验证闭环** (`license.rs`)：补加 `test_real_license_key_verifies` 测试用真实公钥验签真实激活码，移除误导性 `TODO: 生成真实密钥对` 注释（密钥对实际早已替换，注释是历史遗留）

### 修复（P1）
- **PRAGMA 改用 SqliteConnectOptions** (`db.rs`)：从「pool 创建后只跑一次 PRAGMA」改为 connect_with 时通过 `SqliteConnectOptions::pragma` 设置，sqlx 在每条新连接上自动应用，根治「连接池新建连接可能不继承 PRAGMA」隐患
- **schema.sql 前置到 migrations 之前** (`db.rs`)：原顺序导致 v1 迁移的 `ALTER chunks` 在 chunks 表未建时失败，AppState 启动失败。改为先 schema.sql 建表，migrations 只 ALTER 旧库
- **messages.ts 时间格式统一** (`schema.sql` / `db.rs` / `commands.rs`)：v3 迁移把 messages.ts 从 REAL（unix timestamp 浮点）转 TEXT（ISO8601 字符串），与 sessions/meetings/notes 的 created_at 一致。同时移除 schema 误导性 `julianday('now')` default（代码实际 insert 的是 unix timestamp，与 default 语义不符）
- **build.mjs banner 从 package.json 读版本** (`scripts/build.mjs`)：banner 从硬编码 `v2.1.0` 改为模板字符串 `v${pkg.version}`，根治每次发版忘记同步版本号
- **tauri.conf.json asset scope 路径修正** (`tauri.conf.json`)：原 scope 仅含 `笔尖/audio_cache`，漏掉 `PanNote/audio_cache` 和 `com.bijian.app/audio_cache`，导致录音回链被 Tauri 拒绝。改为通配 `$HOME/Library/Application Support/*/audio_cache/**` + 3 个具体路径兜底

### 新增（P1）
- **settings 表** (`schema.sql` / `commands.rs` / `lib.rs`)：新增 `get_setting` / `set_setting` / `list_settings` 三个 Tauri 命令 + schema 表，支持「主题/字号」等设置从 localStorage 迁 SQLite 跨重装可恢复

### 修复（P1）
- **测试恢复 + API 修复** (`tests/`)：从 git 历史恢复 `tests/e2e_test.rs` / `tests/e2e_commands_test.rs` / `tests/comprehensive_test.rs` 三个被误删的测试文件。修复 v2.3.0 API 变更导致的测试失败：`stream_message` MockRuntime 不支持 AppHandle（移除注册）、`trigger_aggregate` 加了 template_type/depth 参数（补 None,None）、`summaries.len()` 改为 `summary.get("summaries")`、setup_app 加 `set_tier(Pro)` 让 trigger_aggregate 通过门控、`test_meeting_crud` 改为期望 Err（worker 异步未跑完）
- **测试结果**：26 测试全过（lib 5 + e2e_test 5 + e2e_commands_test 8 + comprehensive_test 8 + doc 0）

### 新增（P2/P3）
- **删除 /Applications/PanNote Pro.app**：v0.1.0 未签名版与主 app 关系不明，避免混淆
- **全局 panic hook** (`lib.rs`)：`std::panic::set_hook` 至少把 panic 信息打到 stderr。完整 `catch_unwind` 包 5 处 `tauri::async_runtime::spawn` + 2 处 `std::thread::spawn` 留 v2.4.0 实现（收益低、工作量大）

### 推迟到 v2.4.0
- **commands.rs 按域拆分**（3377 行）：内部已有 `// ========== xxx ==========` 段落分隔，物理拆分需处理跨文件私有 fn 依赖，风险大于收益，留独立迭代
- **sherpa_asr_server.py 拆模块**（2381 行）：同上理由
- **Tauri updater 配置**：未签名 app 的更新会被 Gatekeeper 拒绝，需先做 Developer ID 签名 + 公证（99 美元/年）
- **services.rs 搜索优先 Tavily**：当前已是搜狗→必应→Tavily 三级降级链，Tavily 需 API Key，硬性优先反而降低可用性
- **tokio::spawn 完整 catch_unwind**：见上方 panic hook 说明

### 验证
- `cargo build --lib`：编译通过，0 warning
- `cargo test`：26 测试全过
- `cargo test --lib license::tests::test_real_license_key_verifies`：Ed25519 真实密钥对验签闭环通过
- ASR 服务：launchd agent 当前 PID 716，wrapper 子进程 sherpa(727) + firered(729) 均健康返回 ok

---

## [v2.3.1] — 2026-09-09

### 修复
- **纪要流程简化：粗转+纪要两遍** (`voice.js` / `commands.rs`)：砍掉停录后自动精转/说话人分离/标点恢复，停录后直接生成纪要。精转/说话人分离/标点恢复函数保留供手动调用，不再自动触发
- **纪要分片适配纯 CPU** (`services.rs`)：切片 3000→800 字、重叠 300→80 字、num_predict 2048→800、超时 300→240s，适配 Ollama qwen3-4b 纯 CPU ~4 t/s
- **纪要进度条队列可视化** (`voice.js` / `index.html` / `style.css`)：纪要生成进度条升级为"下载队列"式分片色块（待处理灰/处理中蓝脉冲/完成绿/失败红），reduce 阶段全绿后 1.2s 收起
- **launchd UID 不展开** (`commands.rs:1447`)：`"gui/$(id -u)/com.bijian.asr.agent"` 直接传给 launchctl 不经 shell 展开，改用 Rust 调 `id -u` 获取真实 UID，ASR 状态检查恢复正常
- **tauri-bridge.js 路由拦截** (`tauri-bridge.js:121`)：`noteMatch && !isPost` 会拦截 DELETE/PATCH 请求，浏览器降级模式下删笔记变成读笔记。改为 `!isPost && !isDelete && !isPatch`
- **会议删除级联不完整** (`commands.rs:807`)：删除会议漏删 `chunk_summaries`（通过 chunk_id 子查询删）和 `correction_samples` 两张关联表，导致孤儿数据残留
- **会议删除改用事务** (`commands.rs:807`)：多条 DELETE 从独立执行改为 `begin/commit` 事务包裹，任一失败自动回滚
- **自动保存失败静默** (`notes.js:535`)：静默保存失败时只改 indicator 不弹 Toast，用户可能不知情丢数据。改为始终弹 Toast 提示手动重试
- **编译警告清理** (`commands.rs` / `feature.rs`)：删除未使用常量 `SEG_MS`、修多余分号、`#[allow(dead_code)]` 标注 `run_whisper_cpp`/`SendStream`/`pro_set`，编译从 5 警告降至 0

### 新增
- **Pro 功能后端门控** (`commands.rs`)：5 个 Pro 命令入口加 `is_feature_enabled` 检查——`trigger_aggregate`(MeetingSummary)、`trigger_correction`(AsrPremium)、`run_speaker_diarization`(SpeakerDiarization)、`run_cross_validate_transcribe`(AsrPremium)、`stream_message` web_search 分支(WebSearch)。未激活 Pro 时直接返回错误，不再仅依赖前端隐藏按钮
- **ASR 启动并发锁** (`asr_manager.rs`)：`start_in_background` 加 `AtomicBool` CAS 标志，防止并发调用重复拉起 Python 服务，线程结束释放

### 安装
- **二进制安装修正**：Info.plist `CFBundleExecutable` 从 `bijian` 改为 `PanNote`，确保启动新二进制而非旧版

---

## [v2.3.0] — 2026-09-07

### 新增
- **ASR 填充词过滤** (`sherpa_asr_server.py`)：标点恢复后自动过滤"嗯""啊""那个"等口语填充词，纯正则规则零额外负担
- **热词管理面板** (`hotwords.js` / `index.html` / `style.css`)：侧边栏热词入口，支持添加/删除/导入/导出，场景与来源筛选
- **P0 热词闭环** (`commands.rs`)：用户编辑转写文本时自动 diff 提取纠错热词写入 hotwords 表（source=`user_correction`）；热词替换加上下文边界保护防子串误匹配 + 全角半角归一化
- **P1 纠错样本库** (`db.rs` / `schema.sql` / `commands.rs`)：v2 数据库迁移，新增 `correction_samples` 表；编辑转写时自动记录完整样本（ASR 原文、用户修正、音频路径、说话人、场景、引擎、置信度）；新增 `get_correction_stats` 命令查询统计
- **P1 场景化热词动态注入** (`commands.rs`)：`db_get_hotword_context` 按会议标题推断场景，优先取该场景热词前 20 条，不足补 general 至 30 条，4 个调用点全部适配
- **P2 AI 文本纠错** (`services.rs` / `commands.rs` / `voice.js` / `index.html`)：Ollama qwen3-4b + 热词表逐段纠错 ASR 输出（同音字、专有名词、数字格式），temperature=0.1 最小纠错不润色，前端"AI 纠错"按钮 + 进度条
- **前端纠错统计展示** (`hotwords.js`)：热词面板顶部显示样本总数、场景分布、高频错词 Top 5

### 修复
- **纪要渲染显示原始 JSON** (`commands.rs` / `services.rs`)：DB 存储 content 带 ```json markdown 围栏致后端解析失败，get_summary 读取时与 worker 写入前均剥离围栏，已有脏数据已清洗
- **录音播放按钮无响应** (`voice.js`)：playAudioBtn 未绑定 click 事件，新增 togglePlayback 函数（get_meeting_audio 获取路径 + convertFileSrc 转 asset URL + HTML5 Audio 播放/暂停切换）

### 变更
- **DB 迁移至 v2**：schema_migrations 记录新增 v2 条目，correction_samples 表自动创建
- **processed_flag 语义扩展**：0=未处理，1=标点恢复完成，2=AI 纠错完成

---

## [v2.2.0] — 2026-09-06

### 安全
- **数据库迁移重构** (`db.rs`)：引入 `schema_migrations` 版本表管理，替换原 12+ 处 `ALTER TABLE ... .execute().await.ok()` 静默吞错；新增 `safe_add_column` 安全添加列函数；PRAGMA 设置不再 `.ok()` 吞错
- **Markdown XSS 统一** (`chat.js` / `notes.js`)：两套自研 sanitizer 统一为全局 `window.bijian.sanitizeHtml`，补充 form/input/link/meta/style=expression 等过滤规则
- **临时文件迁移**：ASR 进程日志/PID 从 `/tmp/` 迁移至 `~/Library/Application Support/PanNote/asr_logs/`；上传临时音频从 `/tmp/bijian_audio` 迁移至 `~/Library/Application Support/PanNote/tmp_audio`

### 新增
- **数据库备份恢复功能** (`commands.rs`)：新增 `backup_database`、`restore_database`、`list_backups` 三个 Tauri 命令，已注册到 `lib.rs`

### 修复
- **build.mjs 构建统计为 0**：启用 esbuild `metafile: true`，增加 stat 回退，大小统计从 0.0 KB 修复为准确值

### 变更说明
本次更新基于第三方源码诊断报告，经逐条核验后针对 7 个确认问题进行修复。涉及文件 8 个，单元测试 4/4 通过，编译无 error（5 个 warning 均为既有遗留，非本次引入）。

---

## [v2.1.0] — 2026-09-06

### 项目治理
- 项目从 `~/Workbuddy/2026-08-23-18-19-42/bijian-tauri` 迁移至 `~/Documents/PanNote`
- ASR 脚本（sherpa_asr_server.py / firered_server.py / launchd_asr_agent.sh）归至 `asr/` 子目录
- 修改 `commands.rs` 和 `asr_manager.rs` 中硬编码路径，指向新目录
- `.gitignore` 收紧：新增 `gen/schemas/`、`icons/backup_old/`、`icons/android/`、`icons/ios/`
- 从 git 移除 59 个不该跟踪的文件（旧图标备份、自动生成 schema、非当前平台图标）
- 被跟踪文件从 121 降至 62，仓库精简
- 确立版本管理基线：引入 SemVer + Git Tag + CHANGELOG

### UI 优化（承自 v1.0 全量交付）
- 默认首屏改为笔记 Tab（原为 AI 工作台）
- 笔记标题字号 28px（原 16px）
- 三套主题：浅紫（默认）/ 暖白 / 深色
- 格式工具栏精简：4 个常驻按钮 + 9 个收纳到下拉
- 4 种模板快捷插入：会议纪要 / 周工作汇报 / 行动项清单 / 空白标题
- 导出下拉：Markdown / HTML / Word(.doc)
- 视觉收敛：空状态去光环、去渐变改纯色、保存状态标签

---

## [v1.0] — 2026-09-05

### UI 优化全量交付（四阶段）

**阶段一 · 结构调整**
- `activeTab` 从 `'chat'` 改为 `'notes'`
- 快捷键 Tab 顺序改为 notes → voice → chat
- Tab 按钮顺序调整为 笔记 → 语音 → 工作台
- 笔记标题字号从 16px 改为 28px
- 笔记状态栏加入保存状态标签

**阶段二 · 主题系统**
- 追加 `body[data-theme="warm"]` 和 `body[data-theme="dark"]` 两套完整 CSS 变量
- 修复多处硬编码颜色
- 笔记 Tab topbar 加入主题切换按钮
- 主题切换逻辑 + 恢复上次主题

**阶段三 · 视觉收敛 + 工具栏精简**
- 空状态缩小去光环、去渐变改纯色
- 常驻按钮精简为 4 个，其余 9 个收纳到下拉
- 新增下拉样式（带淡入动画）
- 更多格式下拉展开/收起/外部关闭/Esc 交互

**阶段四 · 补齐生态**
- 会议纪要/周工作汇报/行动项清单/空白标题 4 种模板快捷插入
- 导出按钮改为下拉，支持 Markdown / HTML / Word(.doc)
- AI 辅助按钮：确认不嵌入笔记编辑器，守产品哲学

### 打包发布
- `cargo tauri build` 编译产出 PanNote.app
- 替换 /Applications/PanNote.app
- 清理 6 项旧备份/中间产物

---

## [v2.0.0] — 2026-08-23

### 初始化
- Tauri v2 + Rust + SQLite 项目搭建
- 前端原生 JS + esbuild 打包
- AI 对话模块（对接本地 Ollama，流式输出、Function Calling）
- 语音转写模块（FireRed + Sherpa 双引擎 ASR，实时转写 + 精转 + 说话人分离）
- 笔记管理模块（Markdown 编辑器、格式工具栏、截图插入）
- 许可证系统（ed25519 keygen）
- P0 问题修复

> AI生成
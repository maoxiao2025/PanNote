-- 笔尖APP - 数据库 Schema（嵌入 Rust 二进制）

CREATE TABLE IF NOT EXISTS sessions (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '新对话',
    role TEXT NOT NULL DEFAULT 'chat',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS messages (
    id TEXT PRIMARY KEY,
    session_id TEXT NOT NULL,
    role TEXT NOT NULL,
    content TEXT NOT NULL,
    rating INTEGER NOT NULL DEFAULT 0,
    ts TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (session_id) REFERENCES sessions(id)
);

CREATE TABLE IF NOT EXISTS meetings (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL DEFAULT '未命名会议',
    status TEXT NOT NULL DEFAULT 'pending',
    duration INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now')),
    session_dir TEXT NOT NULL DEFAULT '',
    -- v2.4: 笔记×录音合体——会话归属的笔记 id（空 = 独立录音，老数据行为不变）
    note_id TEXT NOT NULL DEFAULT '',
    -- v2.5: 段级统计（状态机改真：状态必须等于事实）
    expected_segments INTEGER NOT NULL DEFAULT -1,
    completed_segments INTEGER NOT NULL DEFAULT 0,
    failed_segments INTEGER NOT NULL DEFAULT 0
);

-- v2.5: 段级转写任务持久化——差集补跑/启动续转/状态机的数据基础
CREATE TABLE IF NOT EXISTS transcription_segments (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    segment_index INTEGER NOT NULL,
    state TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT DEFAULT '',
    started_at TEXT DEFAULT '',
    finished_at TEXT DEFAULT '',
    engine TEXT NOT NULL DEFAULT '',
    UNIQUE(meeting_id, segment_index)
);
CREATE INDEX IF NOT EXISTS idx_tseg_meeting ON transcription_segments(meeting_id);

CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    start_time REAL NOT NULL DEFAULT 0,
    end_time REAL NOT NULL DEFAULT 0,
    transcript TEXT NOT NULL,
    speaker TEXT NOT NULL DEFAULT 'unknown',
    confidence REAL NOT NULL DEFAULT 0,
    word_count INTEGER NOT NULL DEFAULT 0,
    processed_flag INTEGER NOT NULL DEFAULT 0,
    punctuated_text TEXT DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id)
);

CREATE TABLE IF NOT EXISTS chunk_summaries (
    id TEXT PRIMARY KEY,
    chunk_id TEXT NOT NULL,
    content TEXT NOT NULL,
    summary_type TEXT NOT NULL DEFAULT 'chunk',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (chunk_id) REFERENCES chunks(id)
);

CREATE TABLE IF NOT EXISTS final_summaries (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    content TEXT NOT NULL,
    summary_type TEXT NOT NULL DEFAULT 'final',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id)
);

CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    content TEXT NOT NULL DEFAULT '',
    tags TEXT NOT NULL DEFAULT '',
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE VIRTUAL TABLE IF NOT EXISTS notes_fts USING fts5(
    title,
    content,
    tags,
    note_id UNINDEXED,
    tokenize='trigram'
);

CREATE TABLE IF NOT EXISTS evolution_insights (
    id TEXT PRIMARY KEY,
    insight_type TEXT NOT NULL,
    content TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 0.5,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS jobs (
    id TEXT PRIMARY KEY,
    meeting_id TEXT,
    type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    payload TEXT NOT NULL DEFAULT '{}',
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TRIGGER IF NOT EXISTS notes_ai AFTER INSERT ON notes BEGIN
    INSERT INTO notes_fts(rowid, title, content, tags, note_id)
    VALUES (new.rowid, new.title, new.content, new.tags, new.id);
END;

CREATE TRIGGER IF NOT EXISTS notes_ad AFTER DELETE ON notes BEGIN
    DELETE FROM notes_fts WHERE note_id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS notes_au AFTER UPDATE ON notes BEGIN
    DELETE FROM notes_fts WHERE note_id = old.id;
    INSERT INTO notes_fts(rowid, title, content, tags, note_id)
    VALUES (new.rowid, new.title, new.content, new.tags, new.id);
END;

CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id);
CREATE INDEX IF NOT EXISTS idx_chunks_meeting ON chunks(meeting_id);
CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);
CREATE INDEX IF NOT EXISTS idx_jobs_meeting ON jobs(meeting_id);

-- 说话人标签表（用户自定义名称 ↔ 说话人ID）
CREATE TABLE IF NOT EXISTS speakers (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    speaker_id INTEGER NOT NULL,
    label TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id)
);
CREATE INDEX IF NOT EXISTS idx_speakers_meeting ON speakers(meeting_id);

-- P5-4: 声纹库表（跨会议声纹识别）
CREATE TABLE IF NOT EXISTS voiceprints (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    embedding BLOB NOT NULL,          -- numpy embedding 序列化
    sample_count INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- P5-5: 热词/词元库表（ASR 后处理纠正）
-- v3: 增加 scene/frequency/confidence 字段，支持场景词库 + 自进化（第三层）
-- v2.3.2: 应用设置表（跨重装可恢复，从 localStorage 迁 SQLite）
-- 单用户本地应用，key-value 模型足够；value 用 TEXT 兼容字符串/JSON
CREATE TABLE IF NOT EXISTS settings (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS hotwords (
    id TEXT PRIMARY KEY,
    wrong_text TEXT NOT NULL,          -- ASR 可能输出的错误文本
    correct_text TEXT NOT NULL,        -- 正确文本
    priority INTEGER NOT NULL DEFAULT 0,
    source TEXT NOT NULL DEFAULT 'manual',  -- manual / auto / imported / user_correction
    scene TEXT NOT NULL DEFAULT 'general',  -- 所属场景：general / finance / game / daily / medical / legal ...
    frequency INTEGER NOT NULL DEFAULT 1,   -- 命中/使用次数（自进化计数）
    confidence REAL NOT NULL DEFAULT 0.9,   -- 置信度 0~1（用户矫正=1.0，自动=0.6）
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE INDEX IF NOT EXISTS idx_hotwords_wrong ON hotwords(wrong_text);
CREATE INDEX IF NOT EXISTS idx_hotwords_scene ON hotwords(scene);

-- P1: 纠错样本库（用户编辑转写时自动积累，为后续纠错模型做数据准备）
CREATE TABLE IF NOT EXISTS correction_samples (
    id TEXT PRIMARY KEY,
    meeting_id TEXT NOT NULL,
    chunk_id TEXT NOT NULL,
    asr_text TEXT NOT NULL,              -- ASR 原始输出
    corrected_text TEXT NOT NULL,        -- 用户修正后文本
    audio_path TEXT NOT NULL DEFAULT '',  -- 对应音频片段路径
    speaker TEXT NOT NULL DEFAULT 'unknown',
    scene TEXT NOT NULL DEFAULT 'general',
    model TEXT NOT NULL DEFAULT '',       -- 使用的 ASR 引擎名
    confidence REAL NOT NULL DEFAULT 0.0,
    created_at TEXT NOT NULL DEFAULT (datetime('now')),
    FOREIGN KEY (meeting_id) REFERENCES meetings(id),
    FOREIGN KEY (chunk_id) REFERENCES chunks(id)
);
CREATE INDEX IF NOT EXISTS idx_correction_samples_meeting ON correction_samples(meeting_id);
CREATE INDEX IF NOT EXISTS idx_correction_samples_scene ON correction_samples(scene);

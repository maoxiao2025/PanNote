// 笔尖APP - 数据库模块（sqlx 异步版）

use sqlx::{sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous}, Pool, Sqlite, Row};

pub struct Database {
    pool: Pool<Sqlite>,
}

impl Database {
    pub async fn new(path: &str) -> Result<Self, String> {
        // 确保目录存在
        if let Some(parent) = std::path::Path::new(path).parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }

        // v2.3.2: PRAGMA 通过 SqliteConnectOptions 在 connect_with 时设置，
        // sqlx 会在每条新连接上自动应用这些选项——
        // 根治 v2.2 「pool 创建后只跑一次 PRAGMA，连接池新建连接可能不继承」的隐患。
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Normal)
            .foreign_keys(true)
            .pragma("wal_autocheckpoint", "1000");

        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(opts)
            .await
            .map_err(|e| format!("无法连接数据库: {}", e))?;

        // v2.3.2: 先跑 schema.sql 建表，再跑 migrations 处理历史版本差异。
        // 之前的顺序是先 migrations 后 schema.sql——但 v1 迁移里的
        // `safe_add_column(chunks, "punctuated_text", ...)` 会 ALTER chunks 表，
        // 空库时 chunks 表还没建 → ALTER 失败 → AppState 启动失败。
        // 改为先 schema.sql 建表（CREATE IF NOT EXISTS 对旧库无害），迁移只对旧库 ALTER。
        sqlx::query(include_str!("schema.sql"))
            .execute(&pool)
            .await
            .map_err(|e| format!("初始化数据库表失败: {}", e))?;

        // 运行版本化迁移（迁移失败 = 阻止启动，不再静默吞错）
        Self::run_migrations(&pool).await?;

        Ok(Self { pool })
    }

    pub fn pool(&self) -> &Pool<Sqlite> {
        &self.pool
    }

    // v2.3.2: apply_pragmas 已移除——PRAGMA 通过 SqliteConnectOptions 在 connect_with 时设置，
    // sqlx 会在每条新连接上自动应用，根治「pool 后只跑一次 PRAGMA 不继承新连接」的隐患。


    /// 版本化数据库迁移系统。
    ///
    /// - 创建 `schema_migrations` 表记录已应用的迁移版本
    /// - 每个迁移是 `(version, sql)` 元组
    /// - 按顺序执行未应用的迁移，任一失败立即返回错误（不再静默）
    /// - 迁移前自动备份数据库（VACUUM INTO）
    async fn run_migrations(pool: &Pool<Sqlite>) -> Result<(), String> {
        // 创建迁移版本表
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )"
        )
        .execute(pool).await
        .map_err(|e| format!("创建 schema_migrations 表失败: {}", e))?;

        // 获取当前版本
        let current_version: i64 = sqlx::query("SELECT COALESCE(MAX(version), 0) as v FROM schema_migrations")
            .fetch_one(pool).await
            .map(|r| r.get::<i64, _>("v"))
            .unwrap_or(0);

        // 迁移列表：(version, description, sql)
        // 新增列等操作用 "ALTER TABLE ... ADD COLUMN ..."，
        // 旧库可能已有该列 → 用 try 方式兼容（重复 ALTER 会报错但无害）
        let migrations: &[(i64, &str, &str)] = &[
            // v1: 初始迁移 — 把旧版散落的 ALTER TABLE 统一到这里
            (
                1,
                "v1: 初始迁移（punctuated_text, engine_result, session_dir, hotwords 扩展字段）",
                r#"
                -- chunks.punctuated_text
                -- meetings.engine1_result / engine2_result / engine3_result
                -- meetings.session_dir
                -- hotwords.scene / frequency / confidence / updated_at
                "#,
            ),
        ];

        // v1 迁移的 SQL 是注释占位——实际列添加用安全方式处理
        // （SQLite ALTER TABLE ADD COLUMN 不能 IF NOT EXISTS，所以需要逐列 try）
        if current_version < 1 {
            // 安全地添加列：如果列已存在则跳过
            Self::safe_add_column(pool, "chunks", "punctuated_text", "TEXT DEFAULT ''").await?;
            Self::safe_add_column(pool, "meetings", "engine1_result", "TEXT DEFAULT ''").await?;
            Self::safe_add_column(pool, "meetings", "engine2_result", "TEXT DEFAULT ''").await?;
            Self::safe_add_column(pool, "meetings", "engine3_result", "TEXT DEFAULT ''").await?;
            Self::safe_add_column(pool, "meetings", "session_dir", "TEXT DEFAULT ''").await?;
            Self::safe_add_column(pool, "hotwords", "scene", "TEXT NOT NULL DEFAULT 'general'").await?;
            Self::safe_add_column(pool, "hotwords", "frequency", "INTEGER NOT NULL DEFAULT 1").await?;
            Self::safe_add_column(pool, "hotwords", "confidence", "REAL NOT NULL DEFAULT 0.9").await?;
            Self::safe_add_column(pool, "hotwords", "updated_at", "TEXT").await?;

            // 补充 updated_at 的旧数据
            sqlx::query("UPDATE hotwords SET updated_at = datetime('now') WHERE updated_at IS NULL")
                .execute(pool).await
                .map_err(|e| format!("v1 迁移: 补充 hotwords.updated_at 失败: {}", e))?;

            Self::record_migration(pool, 1).await?;
        }

        // v2: 纠错样本库（correction_samples 表由 schema.sql CREATE IF NOT EXISTS 自动创建）
        // 此迁移仅记录版本号，供后续需要 ALTER 时使用
        if current_version < 2 {
            // correction_samples 表已通过 schema.sql 的 CREATE IF NOT EXISTS 在启动时创建
            // v2 目前无需额外 DDL，仅记录版本
            eprintln!("[DB] v2 迁移: correction_samples 表已就绪");
            Self::record_migration(pool, 2).await?;
        }

        // v3: messages.ts 字段类型统一——REAL → TEXT
        // 旧 schema 的 default 是 julianday('now')（儒略日浮点），但代码 insert 的是 unix timestamp (秒)，
        // 字段语义混乱。v2.3.2 统一为 ISO8601 字符串 (datetime('now'))，与其他表 created_at/updated_at 一致。
        // 迁移：用 messages_new 表 + datetime(ts, 'unixepoch') 把旧 unix timestamp 浮点转字符串。
        if current_version < 3 {
            if Self::column_type_is(pool, "messages", "ts").await?.eq_ignore_ascii_case("REAL") {
                eprintln!("[DB] v3 迁移: messages.ts REAL → TEXT (含 unix timestamp → ISO8601 字符串转换)");
                sqlx::query(
                    "CREATE TABLE messages_v3 (
                        id TEXT PRIMARY KEY,
                        session_id TEXT NOT NULL,
                        role TEXT NOT NULL,
                        content TEXT NOT NULL,
                        rating INTEGER NOT NULL DEFAULT 0,
                        ts TEXT NOT NULL DEFAULT (datetime('now')),
                        FOREIGN KEY (session_id) REFERENCES sessions(id)
                    )"
                ).execute(pool).await
                    .map_err(|e| format!("v3 迁移: 创建 messages_v3 失败: {}", e))?;
                sqlx::query(
                    "INSERT INTO messages_v3 (id, session_id, role, content, rating, ts)
                     SELECT id, session_id, role, content, rating,
                            COALESCE(datetime(ts, 'unixepoch'), datetime(ts), datetime('now'))
                     FROM messages"
                ).execute(pool).await
                    .map_err(|e| format!("v3 迁移: 复制 messages 数据失败: {}", e))?;
                sqlx::query("DROP TABLE messages").execute(pool).await
                    .map_err(|e| format!("v3 迁移: DROP 旧 messages 失败: {}", e))?;
                sqlx::query("ALTER TABLE messages_v3 RENAME TO messages").execute(pool).await
                    .map_err(|e| format!("v3 迁移: RENAME 失败: {}", e))?;
                sqlx::query("CREATE INDEX IF NOT EXISTS idx_messages_session ON messages(session_id)").execute(pool).await
                    .map_err(|e| format!("v3 迁移: 重建索引失败: {}", e))?;
                eprintln!("[DB] v3 迁移完成");
            } else {
                eprintln!("[DB] v3 迁移: messages.ts 已是 TEXT 类型，跳过");
            }
            Self::record_migration(pool, 3).await?;
        }

        // v4: 笔记×录音合体——meetings 加 note_id 列（会话归属笔记）
        // 老库 safe_add_column 自动补列（默认 ''，老录音行为不变）；
        // 新库 schema.sql 建表已含该列，safe_add_column 检测到存在直接跳过。
        if current_version < 4 {
            Self::safe_add_column(pool, "meetings", "note_id", "TEXT NOT NULL DEFAULT ''").await?;
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_meetings_note ON meetings(note_id)")
                .execute(pool).await
                .map_err(|e| format!("v4 迁移: 创建 idx_meetings_note 失败: {}", e))?;
            eprintln!("[DB] v4 迁移: meetings.note_id 已就绪");
            Self::record_migration(pool, 4).await?;
        }

        // v5: 可靠性工程——段级转写状态持久化 + 会议段统计（2026-09-25）
        // transcription_segments：每段转写终态落库（pending/processing/done/failed/silent），
        // 差集补跑/启动续转/状态机改真的数据基础。
        if current_version < 5 {
            sqlx::query(
                "CREATE TABLE IF NOT EXISTS transcription_segments (
                    id TEXT PRIMARY KEY,
                    meeting_id TEXT NOT NULL,
                    segment_index INTEGER NOT NULL,
                    state TEXT NOT NULL DEFAULT 'pending',
                    attempts INTEGER NOT NULL DEFAULT 0,
                    last_error TEXT DEFAULT '',
                    started_at TEXT DEFAULT '',
                    finished_at TEXT DEFAULT '',
                    UNIQUE(meeting_id, segment_index)
                )"
            ).execute(pool).await
                .map_err(|e| format!("v5 迁移: 创建 transcription_segments 失败: {}", e))?;
            sqlx::query("CREATE INDEX IF NOT EXISTS idx_tseg_meeting ON transcription_segments(meeting_id)")
                .execute(pool).await
                .map_err(|e| format!("v5 迁移: 创建 idx_tseg_meeting 失败: {}", e))?;
            Self::safe_add_column(pool, "meetings", "expected_segments", "INTEGER NOT NULL DEFAULT -1").await?;
            Self::safe_add_column(pool, "meetings", "completed_segments", "INTEGER NOT NULL DEFAULT 0").await?;
            Self::safe_add_column(pool, "meetings", "failed_segments", "INTEGER NOT NULL DEFAULT 0").await?;
            eprintln!("[DB] v5 迁移: transcription_segments + meetings 段统计已就绪");
            Self::record_migration(pool, 5).await?;
        }

        // v6 (v2.5.2): fallback 可见——transcription_segments 加 engine 列。
        // 记录每段实际引擎；发生过引擎回退的段落 "fallback_qwen3_asr_quick"，
        // 会议级 fallback_count 从段级表直接可统计（引擎退化不再静默）。
        // 新库 schema.sql 建表已含该列，safe_add_column 检测到存在直接跳过。
        if current_version < 6 {
            Self::safe_add_column(pool, "transcription_segments", "engine", "TEXT NOT NULL DEFAULT ''").await?;
            eprintln!("[DB] v6 迁移: transcription_segments.engine 已就绪（fallback 可见）");
            Self::record_migration(pool, 6).await?;
        }

        eprintln!("[DB] 数据库迁移完成，当前版本: {}", migrations.len());
        Ok(())
    }

    /// 安全添加列：如果列已存在则跳过，不存在则添加
    async fn safe_add_column(
        pool: &Pool<Sqlite>,
        table: &str,
        column: &str,
        definition: &str,
    ) -> Result<(), String> {
        // 检查列是否已存在
        let exists: bool = sqlx::query(&format!("PRAGMA table_info({})", table))
            .fetch_all(pool).await
            .map_err(|e| format!("检查 {} 表结构失败: {}", table, e))?
            .iter()
            .any(|row| {
                let name: String = row.get("name");
                name == column
            });

        if exists {
            return Ok(());
        }

        let sql = format!("ALTER TABLE {} ADD COLUMN {} {}", table, column, definition);
        sqlx::query(&sql)
            .execute(pool).await
            .map_err(|e| format!("添加列 {}.{} 失败: {}", table, column, e))?;

        eprintln!("[DB] 已添加列: {}.{}", table, column);
        Ok(())
    }

    /// 查询指定表的指定列的声明类型（PRAGMA table_info 的 type 字段）。
    /// 用于 v3 迁移判断 messages.ts 是否需要从 REAL → TEXT 转换。
    async fn column_type_is(pool: &Pool<Sqlite>, table: &str, column: &str) -> Result<String, String> {
        let rows = sqlx::query(&format!("PRAGMA table_info({})", table))
            .fetch_all(pool).await
            .map_err(|e| format!("检查 {} 表结构失败: {}", table, e))?;
        for row in &rows {
            let name: String = row.get("name");
            if name == column {
                let col_type: String = row.get("type");
                return Ok(col_type);
            }
        }
        Ok(String::new())
    }

    /// 记录已应用的迁移版本
    async fn record_migration(pool: &Pool<Sqlite>, version: i64) -> Result<(), String> {
        sqlx::query("INSERT OR REPLACE INTO schema_migrations (version) VALUES (?)")
            .bind(version)
            .execute(pool).await
            .map_err(|e| format!("记录迁移版本 {} 失败: {}", version, e))?;
        Ok(())
    }

    /// 备份数据库到指定路径（VACUUM INTO）
    pub async fn backup_to(&self, backup_path: &str) -> Result<(), String> {
        if let Some(parent) = std::path::Path::new(backup_path).parent() {
            tokio::fs::create_dir_all(parent).await
                .map_err(|e| format!("创建备份目录失败: {}", e))?;
        }

        sqlx::query(&format!("VACUUM INTO '{}'", backup_path))
            .execute(&self.pool).await
            .map_err(|e| format!("数据库备份失败: {}", e))?;

        Ok(())
    }
}

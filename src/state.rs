// 笔尖APP - 全局状态

use crate::db::Database;
use crate::asr_manager::AsrManager;
use crate::ollama_manager::OllamaManager;
use std::sync::Arc;

pub struct AppState {
    pub db: Arc<Database>,
    pub http_client: reqwest::Client,
    pub asr_manager: Arc<AsrManager>,
    pub ollama_manager: Arc<OllamaManager>,
}

impl AppState {
    pub async fn new(db_path: String) -> Result<Self, String> {
        let db = Arc::new(Database::new(&db_path).await?);
        // 流式请求不设全局 timeout（用 per-request timeout 控制），
        // 避免长时间生成被静默掐断
        let http_client = reqwest::Client::builder()
            .build()
            .map_err(|e| format!("HTTP 客户端初始化失败: {}", e))?;
        let asr_manager = Arc::new(AsrManager::new());
        let ollama_manager = Arc::new(OllamaManager::new());

        Ok(Self { db, http_client, asr_manager, ollama_manager })
    }
}

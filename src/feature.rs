//! 笔尖 APP 功能门控框架
//!
//! 定义免费/Pro 功能集，读取本地激活码状态，控制功能访问。
//! 与 license.rs 配合：license 验证激活码 → feature 根据激活码状态开放功能。

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::RwLock;
use once_cell::sync::Lazy;

/// 功能层级
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FeatureTier {
    /// 免费版
    Free,
    /// Pro 版（已激活）
    Pro,
}

/// 功能标识——所有可控功能的唯一枚举
/// 新增功能时在此添加，并在 FREE_FEATURES / PRO_FEATURES 中归类
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FeatureFlag {
    // ===== 基础功能（免费） =====
    /// AI 对话（本地 Ollama）
    AiChat,
    /// 语音转写（实时转写）
    VoiceTranscribe,
    /// 笔记管理
    Notes,
    /// 基础 ASR 引擎（FireRed）
    AsrBasic,

    // ===== 高级功能（Pro） =====
    /// 精转（Qwen3-ASR 全量精转）
    AsrPremium,
    /// 说话人分离
    SpeakerDiarization,
    /// 会议纪要自动生成
    MeetingSummary,
    /// 联网搜索（web_search）
    WebSearch,
    /// 多模型切换
    MultiModel,
    /// 批量导入音频
    BatchImport,
}

/// 免费版可用功能
/// 2026-09-16: 自研阶段全部功能免费，Pro 门控保留框架但不拦截
const FREE_FEATURES: &[FeatureFlag] = &[
    FeatureFlag::AiChat,
    FeatureFlag::VoiceTranscribe,
    FeatureFlag::Notes,
    FeatureFlag::AsrBasic,
    FeatureFlag::AsrPremium,
    FeatureFlag::SpeakerDiarization,
    FeatureFlag::MeetingSummary,
    FeatureFlag::WebSearch,
    FeatureFlag::MultiModel,
    FeatureFlag::BatchImport,
];

/// Pro 版额外可用功能（保留列表用于未来商业化，当前全部在 FREE 中）
#[allow(dead_code)]
const PRO_FEATURES: &[FeatureFlag] = &[
];

/// 全局功能门控状态
struct FeatureState {
    tier: FeatureTier,
    /// 激活码到期时间（Unix 时间戳，None = 永久或未激活）
    expires_at: Option<i64>,
}

static FEATURE_STATE: Lazy<RwLock<FeatureState>> = Lazy::new(|| {
    RwLock::new(FeatureState {
        tier: FeatureTier::Free,
        expires_at: None,
    })
});

/// 获取所有免费功能集合
fn free_set() -> HashSet<FeatureFlag> {
    FREE_FEATURES.iter().copied().collect()
}

/// 获取所有 Pro 功能集合
#[allow(dead_code)]
fn pro_set() -> HashSet<FeatureFlag> {
    PRO_FEATURES.iter().copied().collect()
}

/// 检查功能是否可用（核心门控逻辑）
pub fn is_feature_enabled(feature: FeatureFlag) -> bool {
    let state = FEATURE_STATE.read().unwrap();
    match state.tier {
        FeatureTier::Pro => {
            // Pro 版：检查是否过期
            if let Some(exp) = state.expires_at {
                let now = chrono::Utc::now().timestamp();
                if now > exp {
                    // 已过期，降级为 Free
                    return free_set().contains(&feature);
                }
            }
            // Pro 未过期：全部功能可用
            true
        }
        FeatureTier::Free => {
            free_set().contains(&feature)
        }
    }
}

/// 设置当前功能层级（由 license 模块调用）
pub fn set_tier(tier: FeatureTier, expires_at: Option<i64>) {
    let mut state = FEATURE_STATE.write().unwrap();
    state.tier = tier;
    state.expires_at = expires_at;
}

/// 获取当前功能层级
pub fn get_tier() -> FeatureTier {
    FEATURE_STATE.read().unwrap().tier
}

/// 获取当前许可状态摘要
#[derive(Debug, Clone, Serialize)]
pub struct LicenseStatus {
    pub tier: FeatureTier,
    pub is_active: bool,
    pub expires_at: Option<i64>,
    pub expires_human: Option<String>,
    pub features: Vec<FeatureInfo>,
}

/// 单个功能信息
#[derive(Debug, Clone, Serialize)]
pub struct FeatureInfo {
    pub flag: FeatureFlag,
    pub name: String,
    pub enabled: bool,
    pub tier: FeatureTier,
}

impl FeatureFlag {
    pub fn name(&self) -> &'static str {
        match self {
            FeatureFlag::AiChat => "AI 对话",
            FeatureFlag::VoiceTranscribe => "语音转写",
            FeatureFlag::Notes => "笔记管理",
            FeatureFlag::AsrBasic => "基础 ASR",
            FeatureFlag::AsrPremium => "精转（Qwen3 全量）",
            FeatureFlag::SpeakerDiarization => "说话人分离",
            FeatureFlag::MeetingSummary => "会议纪要",
            FeatureFlag::WebSearch => "联网搜索",
            FeatureFlag::MultiModel => "多模型切换",
            FeatureFlag::BatchImport => "批量导入",
        }
    }

    pub fn tier_label(&self) -> FeatureTier {
        if free_set().contains(self) {
            FeatureTier::Free
        } else {
            FeatureTier::Pro
        }
    }
}

/// 获取完整许可状态（供前端查询）
pub fn get_license_status() -> LicenseStatus {
    let state = FEATURE_STATE.read().unwrap();
    let now = chrono::Utc::now().timestamp();
    let is_active = match state.tier {
        FeatureTier::Pro => {
            state.expires_at.map_or(true, |exp| now <= exp)
        }
        FeatureTier::Free => false,
    };

    let all_features: Vec<FeatureFlag> = FREE_FEATURES
        .iter()
        .chain(PRO_FEATURES.iter())
        .copied()
        .collect();

    let features = all_features
        .iter()
        .map(|f| FeatureInfo {
            flag: *f,
            name: f.name().to_string(),
            enabled: is_feature_enabled(*f),
            tier: f.tier_label(),
        })
        .collect();

    let expires_human = state.expires_at.map(|ts| {
        chrono::DateTime::from_timestamp(ts, 0)
            .map(|dt| dt.format("%Y-%m-%d").to_string())
            .unwrap_or_else(|| "未知".to_string())
    });

    LicenseStatus {
        tier: state.tier,
        is_active,
        expires_at: state.expires_at,
        expires_human,
        features,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 全部 FeatureFlag 变体（机制测试用，不依赖产品配置）
    const ALL_FLAGS: &[FeatureFlag] = &[
        FeatureFlag::AiChat,
        FeatureFlag::VoiceTranscribe,
        FeatureFlag::Notes,
        FeatureFlag::AsrBasic,
        FeatureFlag::AsrPremium,
        FeatureFlag::SpeakerDiarization,
        FeatureFlag::MeetingSummary,
        FeatureFlag::WebSearch,
        FeatureFlag::MultiModel,
        FeatureFlag::BatchImport,
    ];

    #[test]
    fn test_feature_gating() {
        // 2026-09-25 (v2.5.1) 重写：原断言硬编码「AsrPremium/WebSearch 在 Free 层禁用」，
        // 与 2026-09-16 产品决策（自研阶段全功能免费，门控不拦截）冲突，导致测试随环境漂移失败。
        // 正确姿势：验证「门控机制行为与 FREE_FEATURES 配置一致」，不硬编码具体功能项——
        // 将来商业化收回门控时，本测试依然有效。
        //
        // Free 层：行为必须与 free_set() 配置一致
        set_tier(FeatureTier::Free, None);
        for f in ALL_FLAGS {
            assert_eq!(
                is_feature_enabled(*f),
                free_set().contains(f),
                "Free 层门控行为与 FREE_FEATURES 配置不一致: {:?}",
                f
            );
        }

        // Pro 层（无到期）：全部功能可用
        set_tier(FeatureTier::Pro, None);
        for f in ALL_FLAGS {
            assert!(is_feature_enabled(*f), "Pro 未过期时 {:?} 应可用", f);
        }

        // Pro 层（已过期 → 降级为 Free 行为）
        let past = chrono::Utc::now().timestamp() - 86400; // 昨天过期
        set_tier(FeatureTier::Pro, Some(past));
        for f in ALL_FLAGS {
            assert_eq!(
                is_feature_enabled(*f),
                free_set().contains(f),
                "Pro 过期降级后 {:?} 行为应与 Free 层一致",
                f
            );
        }

        // 收尾恢复默认 Free，避免污染同进程其他测试
        set_tier(FeatureTier::Free, None);
    }
}

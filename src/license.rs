//! 笔尖 APP 本地激活码验证引擎
//!
//! 离线验证方案（与"本地优先"理念一致）：
//! - 激活码 = Base64(payload) + "." + Base64(signature)
//! - payload = JSON { product, tier, expires_at, device_id? }
//! - signature = Ed25519 私钥对 payload 的签名
//! - 验证：用内置公钥验证签名 → 解析 payload → 检查过期 → 激活功能
//!
//! 密钥对由开发者持有：私钥签发激活码，公钥内置在 app 中验证。
//! 用户无需联网，激活码一次输入永久有效（除非有到期时间）。

use serde::{Deserialize, Serialize};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{VerifyingKey, Signature, Verifier};
use chrono::Utc;
use std::path::PathBuf;
use std::fs;

use crate::feature::{self, FeatureTier};

/// 内置 Ed25519 公钥（对应签发激活码的私钥）
/// ⚠️ 这是公钥，可以公开。私钥由开发者保管，不进代码仓库（见 tools/keygen/SECRET_KEY.md）。
/// v2.3.2: 密钥对已生成并验证闭环（见 mod tests::test_real_license_key_verifies），
///         之前注释里的 "TODO: 生成真实密钥对后替换此处公钥" 已完成，移除避免误导。
const EMBEDDED_PUBLIC_KEY: [u8; 32] = [
    0x1b, 0xec, 0x7e, 0xa9, 0x6a, 0x36, 0xf1, 0x9c,
    0xe3, 0xab, 0x11, 0xff, 0xdf, 0x4c, 0xec, 0x05,
    0x5a, 0x58, 0xd9, 0x15, 0x21, 0x4f, 0x4f, 0xbf,
    0x49, 0x22, 0x3a, 0xfe, 0xcc, 0x09, 0x85, 0x1f
];

/// 激活码 payload
#[derive(Debug, Clone, Serialize, Deserialize)]
struct LicensePayload {
    /// 产品标识，固定为 "bijian"
    product: String,
    /// 功能层级："free" 或 "pro"
    tier: String,
    /// 到期时间（Unix 时间戳，None = 永久）
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_at: Option<i64>,
    /// 设备绑定 ID（可选，None = 不绑定设备）
    #[serde(skip_serializing_if = "Option::is_none")]
    device_id: Option<String>,
}

/// 激活码验证结果
#[derive(Debug, Clone, Serialize)]
pub struct ActivationResult {
    pub success: bool,
    pub message: String,
    pub tier: Option<String>,
    pub expires_at: Option<i64>,
    pub expires_human: Option<String>,
}

/// 激活码存储位置
fn license_file_path(app_data_dir: &PathBuf) -> PathBuf {
    app_data_dir.join("license.key")
}

/// 验证激活码
pub fn verify_license_key(key: &str) -> ActivationResult {
    // 激活码格式: base64(payload).base64(signature)
    let parts: Vec<&str> = key.trim().split('.').collect();
    if parts.len() != 2 {
        return ActivationResult {
            success: false,
            message: "激活码格式错误：应为 payload.signature 格式".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        };
    }

    // 解码 payload
    let payload_bytes = match URL_SAFE_NO_PAD.decode(parts[0]) {
        Ok(b) => b,
        Err(_) => return ActivationResult {
            success: false,
            message: "激活码解析失败：payload 解码错误".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        },
    };

    // 解码签名
    let sig_bytes = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(b) => b,
        Err(_) => return ActivationResult {
            success: false,
            message: "激活码解析失败：签名解码错误".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        },
    };

    // 验证签名
    let public_key = match VerifyingKey::from_bytes(&EMBEDDED_PUBLIC_KEY) {
        Ok(k) => k,
        Err(_) => return ActivationResult {
            success: false,
            message: "内置公钥无效（应用配置错误）".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        },
    };

    let sig = match Signature::from_slice(&sig_bytes) {
        Ok(s) => s,
        Err(_) => return ActivationResult {
            success: false,
            message: "激活码签名格式错误".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        },
    };

    // Ed25519 验签
    if public_key.verify(&payload_bytes, &sig).is_err() {
        return ActivationResult {
            success: false,
            message: "激活码签名验证失败：无效的激活码".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        };
    }

    // 解析 payload
    let payload: LicensePayload = match serde_json::from_slice(&payload_bytes) {
        Ok(p) => p,
        Err(_) => return ActivationResult {
            success: false,
            message: "激活码内容解析失败".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        },
    };

    // 校验产品标识
    if payload.product != "bijian" {
        return ActivationResult {
            success: false,
            message: "激活码不属于PanNote产品".to_string(),
            tier: None,
            expires_at: None,
            expires_human: None,
        };
    }

    // 校验到期
    let now = Utc::now().timestamp();
    if let Some(exp) = payload.expires_at {
        if now > exp {
            return ActivationResult {
                success: false,
                message: "激活码已过期".to_string(),
                tier: None,
                expires_at: Some(exp),
                expires_human: Some(format_date(exp)),
            };
        }
    }

    // 激活成功，设置功能层级
    let tier = match payload.tier.as_str() {
        "pro" => {
            feature::set_tier(FeatureTier::Pro, payload.expires_at);
            "pro"
        }
        "free" => {
            feature::set_tier(FeatureTier::Free, payload.expires_at);
            "free"
        }
        _ => {
            return ActivationResult {
                success: false,
                message: format!("未知的功能层级: {}", payload.tier),
                tier: None,
                expires_at: None,
                expires_human: None,
            };
        }
    };

    ActivationResult {
        success: true,
        message: "激活成功".to_string(),
        tier: Some(tier.to_string()),
        expires_at: payload.expires_at,
        expires_human: payload.expires_at.map(format_date),
    }
}

/// 保存激活码到本地文件
pub fn save_license(key: &str, app_data_dir: &PathBuf) -> Result<(), String> {
    let path = license_file_path(app_data_dir);
    fs::write(&path, key.trim())
        .map_err(|e| format!("保存激活码失败: {}", e))?;
    Ok(())
}

/// 从本地文件加载并验证激活码（应用启动时调用）
pub fn load_license(app_data_dir: &PathBuf) -> Option<ActivationResult> {
    let path = license_file_path(app_data_dir);
    if !path.exists() {
        return None;
    }
    let key = fs::read_to_string(&path).ok()?;
    let result = verify_license_key(&key);
    if result.success {
        Some(result)
    } else {
        // 激活码无效，确保降级为 Free
        feature::set_tier(FeatureTier::Free, None);
        Some(result)
    }
}

/// 清除激活码（退出 Pro 模式）
pub fn clear_license(app_data_dir: &PathBuf) -> Result<(), String> {
    let path = license_file_path(app_data_dir);
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("删除激活码失败: {}", e))?;
    }
    feature::set_tier(FeatureTier::Free, None);
    Ok(())
}

fn format_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "未知".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalid_format() {
        let result = verify_license_key("invalid_key");
        assert!(!result.success);
        assert!(result.message.contains("格式错误"));
    }

    #[test]
    fn test_empty_key() {
        let result = verify_license_key("");
        assert!(!result.success);
    }

    #[test]
    fn test_garbage_key() {
        let result = verify_license_key("dGVzdA.dGVzdA");
        assert!(!result.success);
        // 签名验证会失败：payload 解码出 {"test":"..."} 但签名是垃圾
    }

    /// v2.3.2: 验证真实密钥对签发的永久 Pro 激活码能验签通过。
    /// 激活码由 tools/keygen 用 SECRET_KEY.md 中的私钥签发，
    /// 此测试固化「公私钥配对」假设——若 EMBEDDED_PUBLIC_KEY 被无意修改此测试会失败。
    #[test]
    fn test_real_license_key_verifies() {
        // 永久 Pro 激活码：payload = {"product":"bijian","tier":"pro"}
        let key = "eyJwcm9kdWN0IjoiYmlqaWFuIiwidGllciI6InBybyJ9.yt4UH2b-dX1btfY0g0lB0igdJwdj_Qjd_4SslAhrZGQbEJg1jjHo0ydwhH0_FBuQRX6Ey-1Qkz2-A9hi2FAjAw";
        let result = verify_license_key(key);
        assert!(result.success, "真实激活码应验签通过: {}", result.message);
        assert_eq!(result.tier.as_deref(), Some("pro"));
        assert!(result.expires_at.is_none(), "永久激活码无过期时间");
    }
}

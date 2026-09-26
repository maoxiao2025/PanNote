// Tauri build hook:
// 1) 从 tauri.conf.json 读取 version，同步到 Info.plist / Info.plist.template，
//    确保 CFBundleShortVersionString / CFBundleVersion 与工程版本单一来源一致（根治版本漂移）。
// 2) plutil 兜底注入 Info.plist 权限键，规避 tauri-build 偶发 bundle.macOS.infoPlist 丢失。
//
// 注意：macOS 新版 plutil 语法为 `-replace <key> -string <value>`（或 -bool/-integer 等），
// 旧式 `-type string -value <v>` 在本机（macOS 14+）不可用，统一使用新版语法。
use std::path::PathBuf;
use std::process::Command;

fn main() {
    tauri_build::build();

    sync_version_from_tauri_conf();

    let keys = [
        ("NSMicrophoneUsageDescription", "笔尖需要麦克风权限来录制会议音频并进行实时转写"),
        ("NSAudioCaptureUsageDescription",   "笔尖需要音频采集权限来录制会议音频"),
        ("NSScreenCaptureUsageDescription",  "笔尖需要屏幕录制权限来截取屏幕截图插入笔记"),
    ];

    let targets = ["Info.plist", "Info.plist.template"];
    for rel in targets.iter() {
        let p = proj_file(rel);
        if !p.exists() { continue; }
        for (k, v) in keys.iter() {
            // 先 insert（键不存在），失败说明已存在，改用 replace
            if Command::new("plutil")
                .args(["-insert", k, "-string", v, p.to_str().unwrap()])
                .status()
                .map_or(false, |s| !s.success())
            {
                let _ = Command::new("plutil")
                    .args(["-replace", k, "-string", v, p.to_str().unwrap()])
                    .status();
            }
        }
        let _ = Command::new("plutil").args(["-convert", "xml1", p.to_str().unwrap()]).status();
    }
}

/// 从 tauri.conf.json 读取 "version" 字段，写入两个 plist 的版本键。
fn sync_version_from_tauri_conf() {
    let conf = proj_file("tauri.conf.json");
    let Ok(text) = std::fs::read_to_string(&conf) else {
        eprintln!("[build.rs] 未找到 tauri.conf.json，跳过版本同步");
        return;
    };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else {
        eprintln!("[build.rs] tauri.conf.json 解析失败，跳过版本同步");
        return;
    };
    let Some(version) = json.get("version").and_then(|v| v.as_str()) else {
        eprintln!("[build.rs] tauri.conf.json 缺少顶层 version 字段，跳过版本同步");
        return;
    };

    for rel in ["Info.plist", "Info.plist.template"] {
        let p = proj_file(rel);
        if !p.exists() { continue; }
        for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
            if Command::new("plutil")
                .args(["-insert", key, "-string", version, p.to_str().unwrap()])
                .status()
                .map_or(false, |s| !s.success())
            {
                let _ = Command::new("plutil")
                    .args(["-replace", key, "-string", version, p.to_str().unwrap()])
                    .status();
            }
        }
        let _ = Command::new("plutil").args(["-convert", "xml1", p.to_str().unwrap()]).status();
    }
}

fn proj_file(rel: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel)
}
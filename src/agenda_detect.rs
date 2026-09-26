// PanNote - 议程自动检测模块（Rust 原生，移植自 Python agenda_detect.py）
// 用正则引擎扫描会议串场词，自动定位议程切换点
// 零 LLM 依赖，输出 100% 确定

use regex::Regex;
use std::sync::OnceLock;

// 人名模式：1-4字 + 称谓（书记/总/主席）
const PERSON_PATTERN: &str = r"[\u4e00-\u9fa5]{1,4}(?:书记|总|主席)";

// 串场词模式
struct AgendaPattern {
    kind: &'static str,
    regex: Regex,
    person_group: Option<usize>,
}

fn patterns() -> &'static Vec<AgendaPattern> {
    static PATTERNS: OnceLock<Vec<AgendaPattern>> = OnceLock::new();
    PATTERNS.get_or_init(|| vec![
        AgendaPattern {
            kind: "领学",
            regex: Regex::new(&format!(r"请({})领学", PERSON_PATTERN)).unwrap(),
            person_group: Some(1),
        },
        AgendaPattern {
            kind: "议程声明",
            regex: Regex::new(r"第([一二三四五六七八九十]+)项议程[，,，]?\s*(?:开展|进行)?([^，,，。\s]{2,14}?(?:学习|教育|研讨|传达))?").unwrap(),
            person_group: None,
        },
        AgendaPattern {
            kind: "研讨发言",
            regex: Regex::new(&format!(r"(?:首先|接着|邀请|接下来)?[，,、]?\s*请({})(?:进行)?(?:研讨)?发言", PERSON_PATTERN)).unwrap(),
            person_group: Some(1),
        },
        AgendaPattern {
            kind: "总结",
            regex: Regex::new(&format!(r"请({})(?:对|作|进行)?.{{0,12}}?总结", PERSON_PATTERN)).unwrap(),
            person_group: Some(1),
        },
    ])
}

// ASR 谐音归一表
fn alias_map() -> &'static std::collections::HashMap<&'static str, &'static str> {
    static MAP: OnceLock<std::collections::HashMap<&'static str, &'static str>> = OnceLock::new();
    MAP.get_or_init(|| {
        let mut m = std::collections::HashMap::new();
        m.insert("保密总", "宝玉总");
        m.insert("保育总", "宝玉总");
        m.insert("鲍玉总", "宝玉总");
        m
    })
}

fn clean_person(raw: &str) -> String {
    let cleaned: String = raw.chars()
        .filter(|c| !matches!(c, '的' | '了' | '嗯' | '啊' | '好' | '各' | '位' | '下' | '面' | '接' | '来' | '首' | '先' | '邀' | '请' | '，' | ',' | '、' | ' '))
        .collect();
    alias_map().get(cleaned.as_str()).map(|s| s.to_string()).unwrap_or(cleaned)
}

/// 中文数字编号
fn cn_num(n: usize) -> String {
    let digits = ["一", "二", "三", "四", "五", "六", "七", "八", "九", "十"];
    if n <= 9 { digits[n-1].to_string() }
    else if n == 10 { "十".to_string() }
    else if n < 20 { format!("十{}", digits[n-11]) }
    else { n.to_string() }
}

/// 议程锚点
#[derive(Debug, Clone)]
pub struct Anchor {
    pub t: f64,      // 开始时间（秒）
    pub kind: String,
    pub person: String,
}

/// 议程段
#[derive(Debug, Clone)]
pub struct Segment {
    pub t0: f64,
    pub t1: f64,
    pub kind: String,
    pub person: String,
    pub part: String,  // learning / discussion / summary
}

/// 检测阈值
const HEAD_CHARS: usize = 150;
const DEDUP_WINDOW: f64 = 45.0;
const MIN_CHARS: usize = 200;

/// 扫描转写文本，返回议程锚点
pub fn detect_agendas(chunks: &[(f64, String)]) -> Vec<Anchor> {
    let pats = patterns();
    let mut raw_hits: Vec<Anchor> = Vec::new();

    for (t, txt) in chunks {
        let head: String = txt.chars().take(HEAD_CHARS).collect();
        for pat in pats {
            if let Some(m) = pat.regex.captures(&head) {
                let person = if let Some(g) = pat.person_group {
                    clean_person(&m[g])
                } else if pat.kind == "议程声明" {
                    // 提取议题名
                    if let Some(topic) = m.get(2) {
                        let t = topic.as_str();
                        let t = t.strip_prefix("开展").or_else(|| t.strip_prefix("进行")).unwrap_or(t);
                        t.to_string()
                    } else {
                        "专题学习".to_string()
                    }
                } else {
                    String::new()
                };
                raw_hits.push(Anchor {
                    t: *t,
                    kind: pat.kind.to_string(),
                    person,
                });
                break;
            }
        }
    }

    // 排序 + 去重
    raw_hits.sort_by(|a, b| a.t.partial_cmp(&b.t).unwrap_or(std::cmp::Ordering::Equal));
    let mut anchors: Vec<Anchor> = Vec::new();
    for h in raw_hits {
        let dup = anchors.iter().rev().take(3).any(|prev| {
            h.kind == prev.kind && h.person == prev.person && (h.t - prev.t).abs() < DEDUP_WINDOW
        });
        if !dup {
            anchors.push(h);
        }
    }
    anchors
}

/// 从锚点推导段切分，合并过短段
pub fn build_segments(anchors: &[Anchor], chunks: &[(f64, String)]) -> Vec<Segment> {
    if anchors.is_empty() {
        return Vec::new();
    }
    let mut segs: Vec<Segment> = Vec::new();
    for (i, a) in anchors.iter().enumerate() {
        let t1 = if i + 1 < anchors.len() { anchors[i+1].t } else { f64::MAX };
        segs.push(Segment {
            t0: a.t, t1, kind: a.kind.clone(), person: a.person.clone(),
            part: String::new(),
        });
    }

    // 合并微段
    let mut merged: Vec<Segment> = Vec::new();
    for s in segs {
        let char_count: usize = chunks.iter()
            .filter(|(t, _)| *t >= s.t0 && *t < s.t1)
            .map(|(_, txt)| txt.chars().count())
            .sum();
        if char_count < MIN_CHARS && !merged.is_empty() {
            merged.last_mut().unwrap().t1 = s.t1;
        } else {
            merged.push(s);
        }
    }

    classify_part(&mut merged);
    merged
}

/// 议程段归类：学习/研讨/总结
fn classify_part(segs: &mut Vec<Segment>) {
    let first_discussion = segs.iter().find(|s| s.kind == "研讨发言").map(|s| s.t0);
    for s in segs.iter_mut() {
        match first_discussion {
            Some(t) if s.t0 >= t => {
                if s.kind == "总结" { s.part = "summary".into(); }
                else { s.part = "discussion".into(); }
            }
            _ => s.part = "learning".into(),
        }
    }
}

/// 纪要骨架条目
#[derive(Debug, Clone)]
pub struct SkeletonEntry {
    pub level: usize,       // 0=标题 1=一/二 2=（一）（二）
    pub title: String,
    pub seg_idx: Option<usize>,  // 关联段索引
}

/// 生成纪要骨架
pub fn build_skeleton(segs: &[Segment], meeting_title: &str) -> Vec<SkeletonEntry> {
    let mut sk = vec![SkeletonEntry { level: 0, title: format!("{} 会议纪要", meeting_title), seg_idx: None }];

    let has_learning = segs.iter().any(|s| s.part == "learning");
    let has_discussion = segs.iter().any(|s| s.part == "discussion");
    let has_summary = segs.iter().any(|s| s.part == "summary");

    if has_learning {
        sk.push(SkeletonEntry { level: 1, title: "一、学习环节".into(), seg_idx: None });
        let mut n = 0;
        for (i, s) in segs.iter().enumerate() {
            if s.part == "learning" {
                n += 1;
                sk.push(SkeletonEntry { level: 2, title: format!("（{}）{}", cn_num(n), s.person), seg_idx: Some(i) });
            }
        }
    }
    if has_discussion {
        sk.push(SkeletonEntry { level: 1, title: "二、党委成员研讨发言".into(), seg_idx: None });
        let mut n = 0;
        for (i, s) in segs.iter().enumerate() {
            if s.part == "discussion" {
                n += 1;
                sk.push(SkeletonEntry { level: 2, title: format!("（{}）{}发言", cn_num(n), s.person), seg_idx: Some(i) });
            }
        }
    }
    if has_summary {
        let n_disc = segs.iter().filter(|s| s.part == "discussion").count();
        let n = if has_discussion { n_disc + 1 } else { 1 };
        sk.push(SkeletonEntry { level: 2, title: format!("（{}）一把手总结讲话", cn_num(n)), seg_idx: None });
        for (i, s) in segs.iter().enumerate() {
            if s.part == "summary" {
                // 关联到最后一个 level=2 条目
                for entry in sk.iter_mut().rev() {
                    if entry.level == 2 && entry.seg_idx.is_none() && entry.title.contains("总结") {
                        entry.seg_idx = Some(i);
                        break;
                    }
                }
                break;
            }
        }
    }
    sk
}

/// 结构校验：骨架中所有标题必须出现在最终文本中
pub fn validate_structure(final_text: &str, skeleton: &[SkeletonEntry]) -> (bool, Vec<String>) {
    let missing: Vec<String> = skeleton.iter()
        .filter(|e| !e.title.is_empty() && !final_text.contains(&e.title))
        .map(|e| e.title.clone())
        .collect();
    (missing.is_empty(), missing)
}

/// 段摘要拼接：按骨架组装最终纪要
pub fn assemble(skeleton: &[SkeletonEntry], seg_texts: &[String]) -> String {
    let mut lines = Vec::new();
    for entry in skeleton {
        if !entry.title.is_empty() {
            lines.push(entry.title.clone());
        }
        if let Some(idx) = entry.seg_idx {
            if idx < seg_texts.len() && !seg_texts[idx].is_empty() {
                lines.push(seg_texts[idx].clone());
            }
        }
        lines.push(String::new()); // 空行
    }
    lines.join("\n").trim().to_string()
}

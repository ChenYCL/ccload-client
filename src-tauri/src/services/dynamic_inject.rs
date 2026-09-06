//! 动态注入：请求路上的上下文改写（自动插件第二条）。
//!
//! 和另外两个「写上下文」的模块分界：
//! - 系统注入（system_inject）写**磁盘上的全局指令文件** —— 静态，装了就一直在；
//! - 破禁（session_preset）写**会话出生前**的 resume 文件；
//! - 这条在**每个请求经过本地代理时**动态生效 —— 对已经开着的会话、对没有
//!   预设文件机制的 CLI 都有效。
//!
//! 缓存是这个功能的命门：注入的内容和位置必须**逐请求稳定**（同一会话每一轮
//! 注入完全一致），prompt cache 前缀才不会被自己打破。所以规则里没有动态内容、
//! 没有时间戳，位置固定在 system 层的**末尾**（追加，绝不动前面已有的字节），
//! 代理也不用记任何会话状态 —— 内容固定 + 位置固定，天然幂等。
//!
//! 注入点选 system 层而不是伪造对话历史：往历史里塞 assistant 轮要连带拼齐
//! 工具调用块，拼不对的上游当场 400；system 层四家协议都有官方字段。破禁预设
//! 的轮次结构不搬（二期再做预设选择器），取文本意图。与破甲的叠加顺序是
//! 「先注入、后判定」：注入发生在请求进 body 的第一步，破甲重发天然带上。

use serde::{Deserialize, Serialize};

use crate::services::context_floor::alias_key;
use crate::services::rescue::Family;

/// 动态注入的开关与规则。存 `~/.ccload-client/dynamic-inject.json`。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectConfig {
    /// 默认关：注入改的是发往上游的内容，用户要显式选。
    pub enabled: bool,
    pub rules: Vec<InjectRule>,
}

/// 一条注入规则。条件全空 = 所有走代理的聊天请求；多条命中按配置顺序依次追加。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectRule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    /// 匹配的 CLI（`claude-code` 等，见 `detect_cli`）。空 = 任意。
    pub cli: String,
    /// 匹配的模型别名：忽略大小写、忽略 `[1m]` / `(max)` 后缀（和钉住表同一个
    /// 比较 key）。空 = 任意。
    pub model: String,
    /// 追加进 system 层的文本。
    pub text: String,
}

impl Default for InjectRule {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            enabled: true,
            cli: String::new(),
            model: String::new(),
            text: String::new(),
        }
    }
}

impl InjectConfig {
    /// 落盘前的归一化：去空白、丢掉空文本的规则（没有内容的注入没有意义）。
    pub fn normalized(mut self) -> Self {
        self.rules.retain(|r| !r.text.trim().is_empty());
        for r in &mut self.rules {
            r.name = r.name.trim().to_string();
            r.cli = r.cli.trim().to_string();
            r.model = r.model.trim().to_string();
            r.text = r.text.trim().to_string();
        }
        self
    }

    pub fn load(path: &std::path::Path) -> Result<Self, crate::error::AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        let cfg: Self = serde_json::from_str(&raw)
            .map_err(|e| crate::error::AppError::Config(format!("dynamic inject config is corrupt: {e}")))?;
        Ok(cfg.normalized())
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), crate::error::AppError> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| crate::error::AppError::Config(e.to_string()))?;
        crate::services::cli_io::write_atomic(path, &format!("{body}\n"))
    }
}

/// 对一条聊天请求体做注入。返回 `None` = 一个字节都不用改（开关关、不命中、
/// 不是聊天路径、body 解析不了）。聊天路径按 CLI 自己的协议走 —— 内核的协议
/// 转换在这之后，注入必须是 CLI 发出的形状。
pub fn apply(
    cfg: &InjectConfig,
    body: &[u8],
    model: Option<&str>,
    cli: &str,
    path: &str,
) -> Option<Vec<u8>> {
    if !cfg.enabled {
        return None;
    }
    let fam = crate::services::rescue::family(path)?;
    let rules: Vec<&InjectRule> = cfg
        .rules
        .iter()
        .filter(|r| r.enabled && rule_matches(r, model, cli))
        .collect();
    if rules.is_empty() {
        return None;
    }
    let mut v = serde_json::from_slice::<serde_json::Value>(body).ok()?;
    // 不是 JSON 对象（数组、标量）没法挂字段：原样放过，绝不报错。
    if !v.is_object() {
        return None;
    }
    for r in rules {
        inject_system(&mut v, &r.text, fam);
    }
    serde_json::to_vec(&v).ok()
}

fn rule_matches(rule: &InjectRule, model: Option<&str>, cli: &str) -> bool {
    if !rule.cli.is_empty() && rule.cli != cli {
        return false;
    }
    if !rule.model.is_empty() {
        let Some(have) = model else {
            return false;
        };
        if alias_key(&rule.model) != alias_key(have) {
            return false;
        }
    }
    true
}

/// 把一段文本追加进 system 层。四家协议各有官方字段；已有的内容一个字节不动。
fn inject_system(v: &mut serde_json::Value, text: &str, fam: Family) {
    use serde_json::json;
    match fam {
        // system 是字符串就并进去，是块数组就在末尾追加一个 text 块 —— 已有的
        // cache_control 断点前面的内容不变，缓存前缀保得住。
        Family::Anthropic => match v.get_mut("system") {
            Some(serde_json::Value::String(s)) => {
                s.push_str("\n\n");
                s.push_str(text);
            }
            Some(serde_json::Value::Array(arr)) => {
                arr.push(json!({"type": "text", "text": text}));
            }
            _ => {
                v["system"] = json!(text);
            }
        },
        // messages 开头的 system/developer run 之后插入：位置只由 CLI 原本的
        // 头部结构决定，逐请求稳定。
        Family::OpenAi => {
            let Some(arr) = v.get_mut("messages").and_then(|m| m.as_array_mut()) else {
                return;
            };
            let run = arr
                .iter()
                .take_while(|m| {
                    matches!(
                        m.get("role").and_then(|r| r.as_str()),
                        Some("system") | Some("developer")
                    )
                })
                .count();
            arr.insert(run, json!({"role": "system", "content": text}));
        }
        Family::Responses => match v.get_mut("instructions") {
            Some(serde_json::Value::String(s)) => {
                s.push_str("\n\n");
                s.push_str(text);
            }
            _ => {
                v["instructions"] = json!(text);
            }
        },
        Family::Gemini => {
            // systemInstruction 是 Content：{parts:[{text}]}。追加一个 part。
            if v.get("systemInstruction").is_none() {
                v["systemInstruction"] = json!({"parts": [{"text": text}]});
            } else {
                let si = v.get_mut("systemInstruction").unwrap();
                if si.get("parts").and_then(|p| p.as_array()).is_some() {
                    si["parts"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!({"text": text}));
                } else {
                    si["parts"] = json!([{"text": text}]);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn cfg(rules: Vec<InjectRule>) -> InjectConfig {
        InjectConfig {
            enabled: true,
            rules,
        }
    }

    fn rule(cli: &str, model: &str, text: &str) -> InjectRule {
        InjectRule {
            enabled: true,
            cli: cli.into(),
            model: model.into(),
            text: text.into(),
            ..InjectRule::default()
        }
    }

    fn apply_json(body: &str, cfg: &InjectConfig, model: Option<&str>, path: &str) -> Value {
        let out = apply(cfg, body.as_bytes(), model, "claude-code", path).expect("should inject");
        serde_json::from_slice(&out).unwrap()
    }

    /// 注入落在每家协议的 system 层末尾，已有的内容一个字节不动。
    #[test]
    fn text_lands_at_the_end_of_the_system_layer_in_every_protocol_shape() {
        // Anthropic：system 字符串 → 拼接。
        let v = apply_json(
            r#"{"model":"m","system":"你是一个助手。","messages":[]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/messages",
        );
        assert_eq!(v["system"], "你是一个助手。\n\n附加规则");
        // Anthropic：块数组 → 末尾追加 text 块，cache_control 块原样在前。
        let v = apply_json(
            r#"{"system":[{"type":"text","text":"base","cache_control":{"type":"ephemeral"}}]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/messages",
        );
        let arr = v["system"].as_array().unwrap();
        assert_eq!(arr.len(), 2);
        assert_eq!(arr[0]["text"], "base");
        assert!(arr[0].get("cache_control").is_some());
        assert_eq!(arr[1]["text"], "附加规则");
        // OpenAI：插在开头的 system run 之后。
        let v = apply_json(
            r#"{"messages":[{"role":"system","content":"s1"},{"role":"system","content":"s2"},{"role":"user","content":"u"}]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/chat/completions",
        );
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[2]["role"], "system");
        assert_eq!(msgs[2]["content"], "附加规则");
        assert_eq!(msgs[3]["content"], "u");
        // Responses：instructions 拼接。
        let v = apply_json(
            r#"{"instructions":"base","input":[]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/responses",
        );
        assert_eq!(v["instructions"], "base\n\n附加规则");
        // Gemini：systemInstruction.parts 追加。
        let v = apply_json(
            r#"{"contents":[],"systemInstruction":{"parts":[{"text":"base"}]}}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1beta/models/g:generateContent",
        );
        let parts = v["systemInstruction"]["parts"].as_array().unwrap();
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "base");
        assert_eq!(parts[1]["text"], "附加规则");
    }

    /// 没有的字段要造出来，不能因为缺字段而静默丢掉注入。
    #[test]
    fn missing_system_fields_are_created() {
        let v = apply_json(
            r#"{"model":"m","messages":[]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/messages",
        );
        assert_eq!(v["system"], "附加规则");
        let v = apply_json(
            r#"{"input":[]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/responses",
        );
        assert_eq!(v["instructions"], "附加规则");
        let v = apply_json(
            r#"{"contents":[]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1beta/models/g:generateContent",
        );
        assert_eq!(v["systemInstruction"]["parts"][0]["text"], "附加规则");
        let v = apply_json(
            r#"{"messages":[{"role":"user","content":"u"}]}"#,
            &cfg(vec![rule("", "", "附加规则")]),
            Some("m"),
            "/v1/chat/completions",
        );
        assert_eq!(v["messages"][0]["content"], "附加规则");
        assert_eq!(v["messages"][1]["content"], "u");
    }

    /// 匹配口径：CLI 精确等于；模型忽略大小写和 [1m]/(max) 后缀（和钉住表
    /// 同一个 key）。条件空 = 任意。
    #[test]
    fn matchers_narrow_exactly_like_the_pin_table() {
        let c = cfg(vec![rule("claude-code", "Claude-Opus-5", "x")]);
        // CLI 对、模型只差大小写和窗口后缀：命中。
        assert!(apply(&c, b"{}", Some("claude-opus-5[1M]"), "claude-code", "/v1/messages").is_some());
        // CLI 不对：不命中。
        assert!(apply(&c, b"{}", Some("claude-opus-5"), "codex", "/v1/responses").is_none());
        // 模型对不上：不命中。
        assert!(apply(&c, b"{}", Some("gpt-5.6"), "claude-code", "/v1/messages").is_none());
        // 匹配了模型但请求里没有模型名：不命中。
        assert!(apply(&c, b"{}", None, "claude-code", "/v1/messages").is_none());
    }

    /// 多条命中的规则按配置顺序依次追加，不是二选一。
    #[test]
    fn multiple_matching_rules_append_in_config_order() {
        let c = cfg(vec![rule("", "", "第一条"), rule("", "", "第二条")]);
        let v = apply_json(
            r#"{"messages":[{"role":"system","content":"s"}]}"#,
            &c,
            Some("m"),
            "/v1/chat/completions",
        );
        let msgs = v["messages"].as_array().unwrap();
        assert_eq!(msgs[1]["content"], "第一条");
        assert_eq!(msgs[2]["content"], "第二条");
    }

    /// 无事发生的每一种情况都必须原样返回：开关关、规则关、非聊天路径、
    /// body 不是 JSON 对象。
    #[test]
    fn nothing_to_do_passes_the_body_through_untouched() {
        let body = br#"{"model":"m","messages":[]}"#;
        // 开关关。
        assert!(apply(&InjectConfig::default(), body, Some("m"), "claude-code", "/v1/messages").is_none());
        // 规则被单独关掉。
        let mut r = rule("claude-code", "", "x");
        r.enabled = false;
        assert!(apply(&cfg(vec![r]), body, Some("m"), "claude-code", "/v1/messages").is_none());
        // 非聊天路径（内核 admin、模型清单）。
        let c = cfg(vec![rule("", "", "x")]);
        assert!(apply(&c, body, Some("m"), "claude-code", "/v1/models").is_none());
        // body 不是 JSON / 是 JSON 数组。
        assert!(apply(&c, b"not json", Some("m"), "claude-code", "/v1/messages").is_none());
        assert!(apply(&c, b"[1,2]", Some("m"), "claude-code", "/v1/messages").is_none());
    }

    /// 归一化：空文本规则被丢掉、字段去空白；保存后的 reload 结果一致。
    #[test]
    fn normalized_drops_empty_rules_and_trims() {
        let mut r = rule(" claude-code ", "", "  注入  ");
        r.name = " 名字 ".into();
        let c = InjectConfig {
            enabled: true,
            rules: vec![r, InjectRule::default()],
        }
        .normalized();
        assert_eq!(c.rules.len(), 1);
        assert_eq!(c.rules[0].cli, "claude-code");
        assert_eq!(c.rules[0].text, "注入");
        assert_eq!(c.rules[0].name, "名字");
    }
}

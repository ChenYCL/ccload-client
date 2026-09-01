//! Per-alias reasoning / wire-protocol profile.
//!
//! Context windows live in [`super::context_window`] — this file is the other
//! half of "what does this alias actually support". Import and Grok takeover
//! share it so `/effort` and the TUI menu stay in sync with the official
//! catalogs instead of each writer inventing its own list.
//!
//! Sources (do not "improve" the grok-4.5 menu — the official picker has no
//! `xhigh`, and advertising it makes `/effort xhigh` a no-op that looks like
//! a hang):
//!   * Grok Build `models_cache.json` / docs (`/effort`, custom `[model.*]`)
//!   * Claude Code env catalog (`CLAUDE_CODE_EFFORT_LEVEL`,
//!     `ANTHROPIC_*_SUPPORTED_CAPABILITIES`)
//!   * OpenCode `ConfigProviderV1.Model` (`reasoning`, `variants`)
//!   * Codex `model_reasoning_effort`

/// One row of a Grok `/effort` menu (and the OpenCode variant of the same id).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EffortLevel {
    pub id: &'static str,
    pub value: &'static str,
    pub label: &'static str,
    pub description: &'static str,
}

pub const EFFORT_XHIGH: EffortLevel = EffortLevel {
    id: "xhigh",
    value: "xhigh",
    label: "Extra High Effort",
    description: "Highest effort and reasoning level",
};
pub const EFFORT_HIGH: EffortLevel = EffortLevel {
    id: "high",
    value: "high",
    label: "High Effort",
    description: "Higher implementation quality with extensive reasoning",
};
pub const EFFORT_MEDIUM: EffortLevel = EffortLevel {
    id: "medium",
    value: "medium",
    label: "Medium Effort",
    description: "Balanced effort with standard implementation and testing",
};
pub const EFFORT_LOW: EffortLevel = EffortLevel {
    id: "low",
    value: "low",
    label: "Low Effort",
    description: "Quick, fast implementations",
};

/// Official grok-4.6 menu. Default in the live catalog is `high`; we pick
/// `xhigh` for the ccload profile because that's what users running through
/// a proxy actually want, and the menu still lets them drop down.
const MENU_GROK_46: ReasoningMenu = ReasoningMenu {
    default: "xhigh",
    levels: &[EFFORT_XHIGH, EFFORT_HIGH, EFFORT_MEDIUM, EFFORT_LOW],
};

/// Official grok-4.5 menu: no `xhigh`. Advertising it is how `/effort` gets
/// silently dropped — the TUI only offers ids the model declared.
const MENU_GROK_45: ReasoningMenu = ReasoningMenu {
    default: "high",
    levels: &[EFFORT_HIGH, EFFORT_MEDIUM, EFFORT_LOW],
};

/// Claude 4.6+ / GPT-5 / GLM-5 / Kimi and everything else that looks like a
/// reasoning chat model. Claude Code's effort catalog includes `xhigh`.
const MENU_XHIGH: ReasoningMenu = ReasoningMenu {
    default: "high",
    levels: &[EFFORT_XHIGH, EFFORT_HIGH, EFFORT_MEDIUM, EFFORT_LOW],
};

const MENU_HIGH: ReasoningMenu = ReasoningMenu {
    default: "high",
    levels: &[EFFORT_HIGH, EFFORT_MEDIUM, EFFORT_LOW],
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReasoningMenu {
    pub default: &'static str,
    pub levels: &'static [EffortLevel],
}

/// Grok's three backends (docs: 11-custom-models.md). Built-in grok models
/// speak Responses; everything else going through ccLoad is Chat Completions
/// and the kernel converts. Setting `messages` for a Claude alias would POST
/// to `/v1/messages`, which works, but Grok's tool loop is built around the
/// OpenAI shapes.
pub fn grok_api_backend(alias: &str) -> &'static str {
    if family(alias).starts_with("grok") {
        "responses"
    } else {
        "chat_completions"
    }
}

/// `None` for image/audio/embed aliases where sending `reasoning_effort`
/// is wasted traffic and a 400 waiting to happen.
pub fn reasoning_menu(alias: &str) -> Option<ReasoningMenu> {
    let n = family(alias);
    if is_non_reasoning(&n) {
        return None;
    }
    if is_grok_45(&n) {
        return Some(MENU_GROK_45);
    }
    if is_grok_46(&n) {
        return Some(MENU_GROK_46);
    }
    if n.contains("grok") {
        return Some(MENU_HIGH);
    }
    // Claude Code advertises xhigh from Opus 4.7; 4.5 / haiku stay on the
    // three-level menu so we don't offer a level the upstream rejects.
    if n.contains("haiku")
        || (n.contains("claude") && n.contains("4.5"))
        || (n.contains("claude") && n.contains("4-5"))
    {
        return Some(MENU_HIGH);
    }
    if n.contains("claude")
        || n.contains("opus")
        || n.contains("sonnet")
        || n.contains("fable")
        || n.contains("gpt-5")
        || n.contains("o3")
        || n.contains("o4")
        || n.contains("o1")
    {
        return Some(MENU_XHIGH);
    }
    Some(MENU_HIGH)
}

pub fn supports_reasoning(alias: &str) -> bool {
    reasoning_menu(alias).is_some()
}

/// Claude Code's per-slot `*_SUPPORTED_CAPABILITIES` value. Empty when the
/// alias is not a reasoning model — writing `effort` there would make `/effort`
/// show up for a model that can't take it.
pub fn claude_capabilities(alias: &str) -> Option<&'static str> {
    supports_reasoning(alias).then_some("effort,thinking")
}

fn family(alias: &str) -> String {
    let bare = match alias.rsplit_once('/') {
        Some((_, rest)) if !rest.is_empty() => rest,
        _ => alias,
    };
    // Suffix windows (`[1M]`) are about context, not family.
    let trimmed = match bare.rfind('[') {
        Some(i) if bare.ends_with(']') => &bare[..i],
        _ => bare,
    };
    trimmed.to_ascii_lowercase()
}

fn is_grok_46(n: &str) -> bool {
    n.contains("grok-4.6") || n.contains("grok-4-6")
}

fn is_grok_45(n: &str) -> bool {
    n.contains("grok-4.5") || n.contains("grok-4-5")
}

fn is_non_reasoning(n: &str) -> bool {
    n.contains("embed")
        || n.contains("tts")
        || n.contains("whisper")
        || n.contains("dall-e")
        || n.contains("dall_e")
        || n.contains("imagine")
        || n.contains("image")
        || n.contains("moderation")
}

/// Write Grok's `supports_reasoning_effort` + menu onto a `[model.*]` table.
/// No-op for aliases that don't reason — we leave the keys absent so `/effort`
/// stays hidden, matching a built-in that never declared the capability.
pub fn write_grok_effort_menu(table: &mut dyn toml_edit::TableLike, alias: &str) {
    let Some(menu) = reasoning_menu(alias) else {
        return;
    };
    table.insert("supports_reasoning_effort", toml_edit::value(true));
    table.insert("reasoning_effort", toml_edit::value(menu.default));
    let mut arr = toml_edit::Array::new();
    for lvl in menu.levels {
        let mut item = toml_edit::InlineTable::new();
        item.insert("id", lvl.id.into());
        item.insert("value", lvl.value.into());
        item.insert("label", lvl.label.into());
        item.insert("description", lvl.description.into());
        item.insert("default", (lvl.value == menu.default).into());
        arr.push(item);
    }
    table.insert("reasoning_efforts", toml_edit::value(arr));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grok_46_menu_includes_xhigh_and_defaults_to_it() {
        let m = reasoning_menu("grok-4.6").unwrap();
        assert_eq!(m.default, "xhigh");
        assert!(m.levels.iter().any(|l| l.value == "xhigh"));
        assert_eq!(grok_api_backend("grok-4.6"), "responses");
    }

    #[test]
    fn grok_45_menu_has_no_xhigh() {
        let m = reasoning_menu("grok-4.5").unwrap();
        assert_eq!(m.default, "high");
        assert!(!m.levels.iter().any(|l| l.value == "xhigh"));
    }

    #[test]
    fn claude_opus_5_and_glm_flash_are_selectable_reasoning_models() {
        let opus = reasoning_menu("claude-opus-5").unwrap();
        assert!(opus.levels.iter().any(|l| l.value == "xhigh"));
        assert_eq!(grok_api_backend("claude-opus-5"), "chat_completions");
        assert_eq!(claude_capabilities("claude-opus-5"), Some("effort,thinking"));

        let glm = reasoning_menu("glm-5.3-flash[1M]").unwrap();
        assert_eq!(glm.default, "high");
        assert_eq!(grok_api_backend("glm-5.3-flash[1M]"), "chat_completions");
    }

    #[test]
    fn image_and_embed_aliases_do_not_advertise_effort() {
        assert!(reasoning_menu("grok-imagine").is_none());
        assert!(reasoning_menu("text-embedding-3-large").is_none());
        assert!(claude_capabilities("dall-e-3").is_none());
    }

    #[test]
    fn vendor_prefix_and_suffix_do_not_change_the_family() {
        assert_eq!(
            reasoning_menu("xai/grok-4.5").unwrap().levels.len(),
            reasoning_menu("grok-4.5").unwrap().levels.len()
        );
        assert_eq!(
            reasoning_menu("zhipu/glm-5.3-flash[1M]").unwrap().default,
            "high"
        );
    }
}

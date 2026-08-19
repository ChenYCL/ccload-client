//! Multi-layer model fallback, implemented entirely in the client.
//!
//! ccLoad's kernel only rewrites a model once (`redirect_model` is a 1:1
//! mapping) and then retries across *channels*. There is no kernel-side
//! "if fable5 fails try kimi3 then opus-5 then glm5.3" chain. We get that
//! behaviour without touching the kernel by encoding the chain as N
//! channels of descending priority, all sharing the same alias:
//!
//!   alias `fable-5`  →  channel prio 100 (redirects to kimi-k3)
//!                    →  channel prio  90 (redirects to opus-5)
//!                    →  channel prio  80 (redirects to glm-5.3)
//!
//! When the first channel cools (model-level cooldown on the *actual*
//! upstream model), ccLoad's existing selector picks the next-highest
//! priority channel that still serves `fable-5`. Arbitrary depth, no
//! kernel change.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_io::write_atomic;

/// One hop in a fallback chain.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackHop {
    /// The real upstream model this hop should hit.
    pub upstream: String,
    /// Channel this hop is bound to (id after apply, name before).
    #[serde(default)]
    pub channel_id: Option<i64>,
    #[serde(default)]
    pub channel_name: Option<String>,
}

/// A named fallback chain. The first hop is the preferred model; later hops
/// are tried in order when earlier ones cool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FallbackChain {
    /// The alias the CLI asks for (e.g. `fable-5`).
    pub alias: String,
    pub hops: Vec<FallbackHop>,
}

/// Persistable store of every chain the user has configured.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FallbackStore {
    pub chains: Vec<FallbackChain>,
}

impl FallbackStore {
    pub fn load(path: &std::path::Path) -> Result<Self, AppError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(path)?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw)
            .map_err(|e| AppError::Config(format!("fallback store is corrupt: {e}")))
    }

    pub fn save(&self, path: &std::path::Path) -> Result<(), AppError> {
        let body = serde_json::to_string_pretty(self)
            .map_err(|e| AppError::Config(e.to_string()))?;
        // 走 write_atomic 而不是自己拼一个 `.json.tmp`：那个名字由目标路径推出来，
        // 两个并发 save 会落在同一个临时文件上互相截断，rename 过去就是两个文档
        // 首尾相接 —— 也就是 `load` 那句 `fallback store is corrupt` 的来历。
        write_atomic(path, &format!("{body}\n"))
    }

    pub fn upsert(&mut self, chain: FallbackChain) {
        if let Some(existing) = self.chains.iter_mut().find(|c| c.alias == chain.alias) {
            *existing = chain;
        } else {
            self.chains.push(chain);
        }
    }

    pub fn remove(&mut self, alias: &str) {
        self.chains.retain(|c| c.alias != alias);
    }
}

/// Build the `models` array for a channel that should serve `alias` by
/// redirecting it to `upstream`. The alias is what the CLI asks for; the
/// redirect is what the kernel actually sends upstream.
pub fn model_entry(alias: &str, upstream: &str) -> serde_json::Value {
    serde_json::json!({
        "model": alias,
        "redirect_model": upstream,
    })
}

/// Suggested priority for hop `i`. First hop gets the highest number
/// (ccLoad sorts priority DESC), so later hops always lose to earlier ones
/// and we leave room below 80 for the user's own channels.
pub fn hop_priority(i: usize) -> i32 {
    100 - (i as i32) * 10
}

/// Validate a chain before we write anything. Empty aliases or hops would
/// create channels that match every request or none, both of which are
/// worse than rejecting the save.
pub fn validate_chain(chain: &FallbackChain) -> Result<(), AppError> {
    if chain.alias.trim().is_empty() {
        return Err(AppError::Config("alias cannot be empty".into()));
    }
    if chain.hops.is_empty() {
        return Err(AppError::Config("chain must have at least one hop".into()));
    }
    for (i, hop) in chain.hops.iter().enumerate() {
        if hop.upstream.trim().is_empty() {
            return Err(AppError::Config(format!("hop {i} has empty upstream")));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hop_priority_descends() {
        assert_eq!(hop_priority(0), 100);
        assert_eq!(hop_priority(1), 90);
        assert_eq!(hop_priority(2), 80);
    }

    #[test]
    fn rejects_empty_alias() {
        let chain = FallbackChain {
            alias: "  ".into(),
            hops: vec![FallbackHop {
                upstream: "kimi-k3".into(),
                channel_id: None,
                channel_name: None,
            }],
        };
        assert!(validate_chain(&chain).is_err());
    }

    #[test]
    fn upsert_replaces_same_alias() {
        let mut store = FallbackStore::default();
        store.upsert(FallbackChain {
            alias: "fable-5".into(),
            hops: vec![FallbackHop {
                upstream: "kimi-k3".into(),
                channel_id: None,
                channel_name: None,
            }],
        });
        store.upsert(FallbackChain {
            alias: "fable-5".into(),
            hops: vec![
                FallbackHop {
                    upstream: "kimi-k3".into(),
                    channel_id: None,
                    channel_name: None,
                },
                FallbackHop {
                    upstream: "opus-5".into(),
                    channel_id: None,
                    channel_name: None,
                },
            ],
        });
        assert_eq!(store.chains.len(), 1);
        assert_eq!(store.chains[0].hops.len(), 2);
    }
}

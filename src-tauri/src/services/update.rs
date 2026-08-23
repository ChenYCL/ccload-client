//! 壳体自更新检查。
//!
//! # 为什么不用 tauri-plugin-updater
//!
//! 那个插件要的是一份签过名的 `latest.json` 和一对更新签名密钥，而我们的包目前
//! 连代码签名都还没配。在那之前它装不起来，硬上只会在启动时报一个用户看不懂的
//! 签名错误。这里做的是**最轻的一档**：问一次 GitHub Releases，有新版就在侧栏
//! 亮一个按钮，点开浏览器让用户自己下 —— 不自动下载、不自动替换、不动磁盘。
//!
//! # 为什么不能用 `/releases/latest`
//!
//! 和 `kernel-sync.yml` 里同一个坑：那个接口**按定义跳过 prerelease**，而我们
//! 到今天为止发的**全部**是 beta。用它的结果是永远返回 404 或者一个远古正式版，
//! 界面上表现为「从来不提示更新」—— 一个不会响的提醒比没有更坏。所以走
//! `/releases` 列表，自己挑。

use serde::Serialize;

/// 上游仓库。写死是对的：这是**本客户端自己**的发布地址，不是用户可配的东西。
const RELEASES_API: &str = "https://api.github.com/repos/ChenYCL/ccload-client/releases?per_page=10";
const RELEASES_PAGE: &str = "https://github.com/ChenYCL/ccload-client/releases";

/// 一次检查的结果。`available == false` 时其余字段仍然填着，界面上「已是最新」
/// 也要显示当前版本。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct UpdateInfo {
    /// 正在跑的这一版（`tauri.conf.json` 里的，beta 流水线会戳成完整 tag）。
    pub current: String,
    /// 上游最新的一版；一个 release 都没有时是空串。
    pub latest: String,
    /// 那一版的 release 页面。拿不到具体版本就退回 Releases 列表页。
    pub url: String,
    /// 最新版是不是预发布。界面要区分 —— 用正式版的人不该被 beta 追着提示。
    pub prerelease: bool,
    pub available: bool,
}

/// 比较两个版本号，判断 `latest` 是不是比 `current` 新。
///
/// 解析不了就返回 false（而不是 true）：一个解析不出来的版本号说明我们对格式的
/// 假设已经不成立了，这时候提示用户「有新版」是在猜。宁可不提示。
fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |s: &str| semver::Version::parse(s.trim().trim_start_matches('v')).ok();
    match (parse(current), parse(latest)) {
        (Some(cur), Some(new)) => new > cur,
        _ => false,
    }
}

/// 一次失败。
///
/// 分类不是为了好看，是给前端做重试决策用的：**只有 `transport` 值得重试**。
/// 实测这条网络到 api.github.com 五次里能挂两次（SSL_ERROR_SYSCALL、连接超时，
/// 且不经任何代理），单打一次就等于四成概率永远查不到。而 `http` 里最常见的是
/// 限流 403 —— 那个重试只会把每小时 60 次的配额烧得更快。
///
/// 用带 tag 的结构而不是让前端去 `includes("HTTP")` 抠字符串：reqwest 的传输
/// 错误里本来就可能带上 `HTTP/2 stream error`，一撞上就会把该重试的判成不该
/// 重试的，而且这种错判是静默的 —— 正是这个功能已经栽过一次的那种坑。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "message", rename_all = "snake_case")]
pub enum CheckError {
    /// 根本没连上：断网、DNS、TLS 握手失败、超时。重试有意义。
    Transport(String),
    /// GitHub 答了话，但不是 2xx（限流 403、404……）。重试没意义。
    Http(String),
    /// 连上了也是 2xx，但回来的东西读不成我们要的形状。重试没意义。
    Malformed(String),
}

impl std::fmt::Display for CheckError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(m) | Self::Http(m) | Self::Malformed(m) => f.write_str(m),
        }
    }
}

/// 问一次 GitHub。
///
/// 失败一律往上抛：这个功能是锦上添花，网络不通、限流、断网都不该让侧栏报错弹窗
/// —— 侧栏把错误当成「这次没查到」静默处理，设置页才把原因摊开给人看。
pub async fn check(current: &str) -> Result<UpdateInfo, CheckError> {
    let none = |latest: String, url: String, prerelease: bool| UpdateInfo {
        current: current.to_string(),
        available: is_newer(current, &latest),
        latest,
        url,
        prerelease,
    };

    // 这里**不关代理**：GitHub 在外网，和内核那条「多半在 127.0.0.1」正好相反。
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        // 构建客户端失败是本地的事（TLS 后端初始化不了），重试也是一样的结果。
        .map_err(|e| CheckError::Malformed(format!("build http client: {e}")))?;

    let resp = client
        .get(RELEASES_API)
        // GitHub 对没有 User-Agent 的请求直接回 403，这一行不是可选的。
        .header("user-agent", "ccload-client")
        .header("accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| CheckError::Transport(format!("cannot reach GitHub: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        // 403 基本都是未认证的限流（每小时 60 次/IP）。说清楚，免得被当成网络故障。
        let hint = if status.as_u16() == 403 {
            "（多半是 GitHub 对未认证请求的限流，每小时 60 次）"
        } else {
            ""
        };
        return Err(CheckError::Http(format!(
            "GitHub returned HTTP {status}{hint}"
        )));
    }

    let releases: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| CheckError::Malformed(format!("bad GitHub response: {e}")))?;

    // 草稿要跳过：它对非 owner 不可见，拿它去提示别人「有新版」会指向一个 404。
    // 列表接口按创建时间倒序，所以第一个非草稿就是最新的那一个。
    let newest = releases
        .iter()
        .find(|r| !r.get("draft").and_then(serde_json::Value::as_bool).unwrap_or(false));

    let Some(rel) = newest else {
        return Ok(none(String::new(), RELEASES_PAGE.to_string(), false));
    };

    let tag = rel
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let url = rel
        .get("html_url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(RELEASES_PAGE)
        .to_string();
    let prerelease = rel
        .get("prerelease")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);

    Ok(none(tag, url, prerelease))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 我们发的全是 `0.1.0-beta.<日期>.<当天第几次>`，这一档必须比对。
    #[test]
    fn beta_ordering_follows_semver_not_string_order() {
        assert!(is_newer("0.1.0-beta.20260822.4", "0.1.0-beta.20260822.5"));
        // 跨天：字典序在这里恰好也对，但数值比才是我们依赖的语义
        assert!(is_newer("0.1.0-beta.20260821.18", "0.1.0-beta.20260822.1"));
        // **字典序会判反的那一对**：段内是数字，10 比 9 大
        assert!(is_newer("0.1.0-beta.20260822.9", "0.1.0-beta.20260822.10"));
        assert!(!is_newer("0.1.0-beta.20260822.10", "0.1.0-beta.20260822.9"));
    }

    /// 同号正式版比任何 beta 新 —— semver 的规矩，也正是我们想要的：
    /// 装着 0.1.0-beta.x 的人应该被提示升到 0.1.0。
    #[test]
    fn a_release_beats_its_own_prereleases() {
        assert!(is_newer("0.1.0-beta.20260822.4", "0.1.0"));
        assert!(!is_newer("0.1.0", "0.1.0-beta.20260822.4"));
    }

    #[test]
    fn same_version_is_not_an_update() {
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta.20260822.4", "0.1.0-beta.20260822.4"));
        // 带不带 v 前缀都要认：tag 是 `v0.1.1`，getVersion() 给的是 `0.1.1`
        assert!(!is_newer("0.1.0", "v0.1.0"));
        assert!(is_newer("0.1.0", "v0.1.1"));
    }

    /// 降级不提示。上游撤下一版、或者用户装的就是比线上新的自编译包，
    /// 都不该被劝「更新」到一个更旧的版本。
    #[test]
    fn older_upstream_is_not_an_update() {
        assert!(!is_newer("0.2.0", "0.1.9"));
        assert!(!is_newer("0.1.0", "0.0.9"));
    }

    /// 解析不了就当没有更新。宁可不提示，也不要凭猜测让用户去下一个包。
    #[test]
    fn unparseable_versions_never_claim_an_update() {
        assert!(!is_newer("0.1.0", ""));
        assert!(!is_newer("", "0.1.1"));
        assert!(!is_newer("0.1.0", "nightly"));
        assert!(!is_newer("dev", "0.1.1"));
    }

    /// 错误的 JSON 形状是跨语言契约：前端靠 `kind` 决定重不重试（只重 transport）。
    /// 改这里等于改前端的重试逻辑，而那种改动一旦判反是**静默**的 —— 用户只会
    /// 发现「更新提示又不出现了」，跟这次一模一样。所以把形状钉死在测试里。
    #[test]
    fn error_json_shape_matches_the_frontend_contract() {
        let cases = [
            (CheckError::Transport("boom".into()), "transport"),
            (CheckError::Http("429".into()), "http"),
            (CheckError::Malformed("nope".into()), "malformed"),
        ];
        for (err, kind) in cases {
            let v = serde_json::to_value(&err).expect("serialize");
            assert_eq!(v["kind"], kind, "kind tag drifted for {err:?}");
            assert_eq!(v["message"], err.to_string(), "message field drifted");
        }
    }
}

/// 真打一次 GitHub。
///
/// `#[ignore]`，要 `cargo test --lib update::net_probe -- --ignored --nocapture`
/// 才跑 —— 断网或撞上限流时它会红，而那不是代码的问题，不该拦住 CI。
///
/// 留着是有原因的：这个功能上线后第一次报「按钮从来没出现过」，光看代码没法
/// 判断是 Rust 查错了、GitHub 返回变了、还是前端没渲染。跑一下这个就把前两种
/// 一次排除掉（当时的结论是后端好的，坏在前端的刷新策略）。
#[cfg(test)]
mod net_probe {
    #[tokio::test]
    #[ignore]
    async fn hits_github_for_real() {
        // 一个真实发布过的旧版本：无论线上走到哪一版，它都该被判定为「有更新」。
        let r = super::check("0.1.0-beta.20260823.1").await;
        println!("{r:#?}");
        let info = r.expect("check() should reach GitHub");
        assert!(!info.latest.is_empty(), "latest tag must not be empty");
        assert!(info.available, "an old beta must be behind the live release");
    }
}

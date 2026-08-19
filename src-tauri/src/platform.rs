//! 平台相关的启动期调整。

/// 关掉 macOS 对本应用的自动大写与智能替换。
///
/// 这个界面里几乎每一个输入框装的都是模型 ID、环境变量名、shell 命令、URL：
/// `claude-opus-5` 被首字母大写成 `Claude-opus-5` 就是一条写不进去的配置，
/// 直引号被替换成弯引号则会让 JSON/TOML 直接解析失败。
///
/// HTML 上的 `autocapitalize` / `autocorrect` 属性在桌面 WebKit 上只是软键盘
/// 提示，管不到这一层 —— 真正生效的开关是 NSUserDefaults 里的这几个键，而且
/// 必须在 WebView 建出来之前写，之后再改要等下次启动。
///
/// 只改本应用的 domain，不碰用户的全局设置。
#[cfg(target_os = "macos")]
pub fn disable_automatic_text_substitutions() {
    use objc2_foundation::{NSString, NSUserDefaults};

    const KEYS: &[&str] = &[
        "NSAutomaticCapitalizationEnabled",
        "NSAutomaticSpellingCorrectionEnabled",
        "NSAutomaticQuoteSubstitutionEnabled",
        "NSAutomaticDashSubstitutionEnabled",
        "NSAutomaticPeriodSubstitutionEnabled",
        "NSAutomaticTextReplacementEnabled",
    ];

    let defaults = NSUserDefaults::standardUserDefaults();
    for key in KEYS {
        defaults.setBool_forKey(false, &NSString::from_str(key));
    }
}

#[cfg(not(target_os = "macos"))]
pub fn disable_automatic_text_substitutions() {}

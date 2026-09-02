//! 系统注入：把一段受管的说明写进每个 CLI 的**全局指令文件**。
//!
//! # 要解决什么
//!
//! 我们给 CLI 装了 `ccload-vision` 这个 MCP，可是装了不等于会被用。宿主模型
//! 只看得到工具名和一句 description，遇到「用户贴了张图」时会不会想起来调它，
//! 全看运气 —— 尤其是本来就不支持多模态的模型，它甚至不知道自己"看不见"。
//! 一句写在系统提示里的规则比工具描述强得多：**你看不见图片，凡是遇到图片
//! 就调这几个工具**。
//!
//! `ccload-image`（生图）是同一个问题的另一面：模型不会主动想到「这张图我可以
//! 自己画出来」，默认反应是让用户去找别的工具。所以那一段写的是**什么场合该
//! 想起它**。
//!
//! 每个 CLI 都有一个「全局 markdown 指令文件」，启动时无条件读进系统提示：
//!
//! | CLI         | 文件                            |
//! |-------------|---------------------------------|
//! | Claude Code | `~/.claude/CLAUDE.md`           |
//! | Codex       | `~/.codex/AGENTS.md`            |
//! | Gemini CLI  | `~/.gemini/GEMINI.md`           |
//! | Grok Build  | `~/.grok/AGENTS.md`             |
//! | OpenCode    | `~/.config/opencode/AGENTS.md`  |
//!
//! 这几条路径不是猜的：Grok 的 `~/.grok/README.md` 明写了它按
//! `~/.grok/` → repo root → cwd 的顺序找 `AGENTS.md` 一类文件并追加进系统提示，
//! 其余四家在本机的配置目录里都能直接看到对应文件。
//!
//! # 为什么是「标记块」而不是整份文件
//!
//! 这些文件是用户自己的地盘 —— `~/.claude/CLAUDE.md` 里往往是攒了几个月的
//! 个人规则。整块覆盖会把它们抹掉，而那是不可逆的（不像 settings.json，
//! 用户心里有数它是工具在管）。所以只认我们自己那对标记之间的内容：
//!
//! ```text
//! <!-- ccload:begin --> … <!-- ccload:end -->
//! ```
//!
//! 重复写入替换块内，块外一个字节都不动；移除只删这一段。这条不变量由
//! `replace_block` 保证，也是这个模块最需要被测试盯住的地方。

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::services::cli_backup::BackupStore;
use crate::services::cli_io::write_atomic;
use crate::services::cli_types::{CliTarget, ConfigRoot};

/// 标记用 HTML 注释：五家读的都是 markdown，注释在渲染和阅读时都不碍事，
/// 而且模型看到它也知道这段是工具生成的。
const BEGIN: &str = "<!-- ccload:begin 由 ccLoad 客户端管理，块内改动会在下次写入时被覆盖 -->";
const END: &str = "<!-- ccload:end -->";

/// Grok 明写每个规则文件截断到 10000 字符。别家没写上限，但按最严的那个提醒，
/// 免得用户在 Grok 上被静默截断还不知道。
pub const SOFT_MAX_CHARS: usize = 10_000;

/// 三个小节的标题**同时是解析锚点** —— 界面靠它们把已写进文件的块拆回
/// 「哪几项勾着 + 用户自己写了什么」。
///
/// 改这三行等于把用户机器上所有已注入的块变成「不认识的内容」：勾选框回显成
/// 没勾，而那段旧文字会被当成用户自己写的原样保留下来，下一次写入就变成一段
/// 旧的 + 一段新的，两份说明并存。要改标题就得同时在 `parse_block` 里留下旧
/// 标题的别名。（真发生过，见 `an_older_wording_still_counts_as_the_same_section`。）
const VISION_HEADING: &str = "## 图片处理（ccLoad 视觉辅助）";
const IMAGE_HEADING: &str = "## 生成与修改图片（ccLoad 生图）";
const TOOLS_HEADING: &str = "## 本机可用的工具";

/// 每一小节前面的分段标记。
///
/// 光有标题不够：用户自己那段写在最后、又没有标题，光按标题切的话它会被算进
/// 前一节的正文里，回显时丢掉 —— 而丢掉的下一步是按「更新」把它从磁盘上也
/// 抹掉。有了这行标记，边界是确定的，不用去猜哪一段是谁写的。
const MARK_VISION: &str = "<!-- ccload:vision -->";
const MARK_IMAGE: &str = "<!-- ccload:image -->";
const MARK_TOOLS: &str = "<!-- ccload:tools -->";
const MARK_CUSTOM: &str = "<!-- ccload:custom -->";

fn marker_kind(line: &str) -> Option<Segment> {
    match line.trim() {
        MARK_VISION => Some(Segment::Vision),
        MARK_IMAGE => Some(Segment::Image),
        MARK_TOOLS => Some(Segment::Tools),
        MARK_CUSTOM => Some(Segment::Custom),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Segment {
    Vision,
    Image,
    Tools,
    Custom,
}

/// 各 CLI 的全局指令文件（相对 home）。
pub fn instructions_path(target: CliTarget) -> &'static str {
    match target {
        CliTarget::ClaudeCode => ".claude/CLAUDE.md",
        CliTarget::Codex => ".codex/AGENTS.md",
        CliTarget::GeminiCli => ".gemini/GEMINI.md",
        CliTarget::GrokBuild => ".grok/AGENTS.md",
        CliTarget::OpenCode => ".config/opencode/AGENTS.md",
    }
}

/// 要注入哪些内容。每一项都是用户可关的 —— 注入进系统提示的东西会花掉每一次
/// 请求的 token，不该由我们替用户决定全都要。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectSpec {
    /// 视觉工具用法。装了 `ccload-vision` 却没人告诉模型该用，是这个功能的由来。
    pub vision: bool,
    /// 生图工具用法。同上，装了 `ccload-image` 也得有人告诉模型它能画。
    pub image: bool,
    /// 给已装的第三方扩展写的「什么时候用它」。
    pub tools: Vec<ToolNote>,
    /// 用户自己的规则，原样写进块里。
    pub custom: String,
}

/// 一条第三方扩展的用法说明。
///
/// 为什么需要它：MCP 的工具描述只有一句话，而且是写给「这个工具是干什么的」，
/// 不是「什么时候该想起它」。装了 codegraph 不等于模型会在改代码前先去查调用链
/// —— 那句话得有人写下来。用户为 Claude Code 手写过的这类说明，往往只存在于
/// `~/.claude/CLAUDE.md` 里，另外四家 CLI 一个字都看不到；这里让它写一次、推五家。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolNote {
    /// 扩展 id（MCP 服务器名 / skill 目录名 / agent 文件名）。
    pub name: String,
    /// 什么时候用它。空的条目直接跳过 —— 只写名字对模型没有任何增量信息，
    /// 反而白占 token。
    pub note: String,
}

impl InjectSpec {
    fn live_tools(&self) -> Vec<&ToolNote> {
        self.tools
            .iter()
            .filter(|t| !t.name.trim().is_empty() && !t.note.trim().is_empty())
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        !self.vision && !self.image && self.custom.trim().is_empty() && self.live_tools().is_empty()
    }
}

/// 一个 CLI 的当前注入状态。
#[derive(Debug, Clone, Serialize)]
pub struct InjectState {
    pub target: CliTarget,
    pub label: &'static str,
    /// 全局指令文件的绝对路径，界面上要显示 —— 用户得知道我们在改哪个文件。
    pub path: String,
    pub exists: bool,
    /// 已经注入过（文件里有我们那对标记）。
    pub injected: bool,
    /// 块内现有内容，用来回显。
    pub block: Option<String>,
    /// 块里解析出来的 spec —— 界面上的勾选框、工具说明、用户那段都从它回显。
    /// 靠磁盘上的真值，不靠按钮的记忆。
    pub spec: Option<InjectSpec>,
    /// 装着的是**旧版本的措辞**：内容还在生效，但和这一版渲染出来的不一样，
    /// 按一下「更新」就能刷新。不标出来的话用户没有任何线索知道该按它。
    pub outdated: bool,
    /// 整个文件的字符数。Grok 到 10000 会截断，界面上据此提醒。
    pub chars: usize,
}

/// 视觉工具那一段。
///
/// 措辞上刻意用「你看不见图片」这种绝对句式而不是「如果你看不见」：让模型
/// 自己判断有没有视觉能力是不可靠的 —— 它经常以为自己能看。这段只有在用户
/// 确实装了视觉 MCP 且当前模型不是多模态时才该开，所以由界面负责把话说清楚，
/// 这里就按「已经确认需要」来写。
///
/// 工具名和 `vision_mcp::tool_specs` 必须一致；对不上就是让模型去调一个不存在
/// 的工具，比不写还糟。
fn vision_section() -> String {
    format!(
        "\
{VISION_HEADING}

你**看不见图片**。本机装了 MCP 服务器 `{server}`，它把图片交给一个多模态模型
再把文字结果给你。凡是遇到图片，必须调用下面的工具，不要猜测图片内容，也不要
让用户改用文字描述：

- `describe_image` —— 看懂一张图（截图、照片、示意图、图表）。
- `read_image_text` —— 逐字抄下图上的文字。报错截图、终端输出、日志、代码、
  表单一律用它，`describe_image` 会概括，而这些场景要的是原文。
- `compare_images` —— 比对两张图的差异。改动前后、视觉回归用它。
- `list_pasted_images` —— 列出用户刚贴进来的图和它们在磁盘上的路径。
- `describe_screen` —— 截取当前屏幕再描述。用户说「看看这个」却没贴文件时用
  它（仅 macOS）。

取图参数：有本地路径用 `path`，有网址用 `url`。对话里只有 `[Image 1]` 这种占位
符、没有路径时，把 `image` 设成 1（对应 `[Image 1]`），**不要让用户把图另存
到 Downloads 再把路径发回来** —— 图已经在会话目录里，工具自己能找到。一张图、
或用户说「看看这张」时 `image` 可以省略。",
        server = crate::services::vision_mcp::MCP_NAME,
    )
}

/// 生图工具那一段。
///
/// 和视觉那段同一个道理，只是反过来：模型不会主动想到「我可以自己把这张图画
/// 出来」，它默认的反应是让用户去找设计师或者给一段 SVG。所以这里要把「什么
/// 场合该想起它」写死 —— 图标、精灵图、贴图、UI 草图、占位素材。
///
/// 还必须写清两件容易出错的事：
///   * 结果是**磁盘路径**，不是图本身。模型看不见它，要看得接着调
///     `describe_image`（前提是视觉那个 MCP 也装了）。
///   * 改图走 `edit_image`，原图不会被动。
///
/// 工具名和 `image_mcp::tool_specs` 必须一致；对不上就是让模型去调一个不存在
/// 的工具，比不写还糟。
fn image_section() -> String {
    format!(
        "\
{IMAGE_HEADING}

本机装了 MCP 服务器 `{server}`，你可以**自己把图画出来**，不要让用户去找别的
工具，也不要用 SVG/ASCII 凑数：

- `generate_image` —— 从一段描述生成一张新图。图标、精灵图、贴图、按钮、
  Logo、插画、UI 草图、占位素材，凡是「现在还不存在的图」都用它。
- `edit_image` —— 按指令改一张已有的图，结果**另存为新文件，原图不动**。
  「背景换成夜晚」「把这个草稿画完」「去掉水印」，以及把几张参考图合成一张
  （用 `extra_paths` 带上其余的图）都是它。

要点：

- 结果回给你的是**保存路径**，不是图本身 —— 你看不见它。需要确认画成什么样
  再拿给用户，就用那个路径调 `describe_image`。
- 提示词写具体：主体、风格、构图、配色、背景是否透明。「一个图标」会得到
  一张没法用的图。
- 尺寸用 `size`：宽高比（`1:1@2k`、`16:9`，宽高比只有 `1:1 16:9 9:16 3:2 2:3`
  五种）和像素（`1024x1536`）两种写法都认，服务器会换成当前模型收得下的形式。
  不确定就别传，默认值是对的。
- 改图时如果对话里只有 `[Image 1]` 没有路径，把 `image` 设成 1，**不要让用户
  把图另存一份再把路径发回来**。",
        server = crate::services::image_mcp::MCP_NAME,
    )
}

/// 拼出块内内容（不含标记本身）。
pub fn render_block(spec: &InjectSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    if spec.vision {
        parts.push(format!("{MARK_VISION}\n{}", vision_section()));
    }
    if spec.image {
        parts.push(format!("{MARK_IMAGE}\n{}", image_section()));
    }
    let tools = spec.live_tools();
    if !tools.is_empty() {
        let mut sec = format!("{MARK_TOOLS}\n{TOOLS_HEADING}\n\n");
        for t in tools {
            sec.push_str(&format!("- `{}` {TOOL_SEP} {}\n", t.name.trim(), t.note.trim()));
        }
        parts.push(sec.trim_end().to_string());
    }
    let custom = spec.custom.trim();
    if !custom.is_empty() {
        parts.push(format!("{MARK_CUSTOM}\n{custom}"));
    }
    parts.join("\n\n")
}

/// 工具条目里名字和说明之间的分隔。解析时按**第一个**它来切，说明本身再出现
/// 一次也不会被截断。
const TOOL_SEP: &str = "——";

/// 把块内内容拆回一个 `InjectSpec` —— 界面上的勾选状态、每条工具说明、用户
/// 自己那段，全靠它回显。
///
/// # 为什么不在前端按「渲染一遍再看包不包含」来判断
///
/// 那是这个函数替换掉的做法，它有一个必然发生的失效：**我们自己改一个字，
/// 判断就失灵**。用户机器上的块是上一个版本写进去的，和这一版渲染出来的文本
/// 不逐字相同，于是「视觉」被判成没勾，整段旧文字被当成用户手写内容 —— 再按
/// 一次「更新」就会写出一段旧的加一段新的。措辞是会改的，标记不会。
///
/// 认不出来的部分一律进 `custom` 并**原样保留**：宁可把我们生成的东西错当成
/// 用户的（顶多多留一段），也不能把用户手写的规则当成我们的给覆盖掉。
pub fn parse_block(block: &str) -> InjectSpec {
    let mut spec = InjectSpec::default();
    let mut custom: Vec<String> = Vec::new();

    // 先按分段标记切。没有任何标记 = 老版本写的块，走标题兜底。
    let mut segs: Vec<(Option<Segment>, String)> = Vec::new();
    let mut cur: Option<Segment> = None;
    let mut buf = String::new();
    for line in block.split_inclusive('\n') {
        if let Some(kind) = marker_kind(line) {
            segs.push((cur, std::mem::take(&mut buf)));
            cur = Some(kind);
            continue;
        }
        buf.push_str(line);
    }
    segs.push((cur, buf));
    if segs.iter().all(|(k, _)| k.is_none()) {
        return parse_legacy(block);
    }

    for (kind, text) in segs {
        match kind {
            Some(Segment::Vision) => spec.vision = true,
            Some(Segment::Image) => spec.image = true,
            Some(Segment::Tools) => spec.tools.extend(parse_tool_notes(&text)),
            Some(Segment::Custom) => {
                let t = text.trim();
                if !t.is_empty() {
                    custom.push(t.to_string());
                }
            }
            // 标记之前的内容：可能是上个版本留下的、也可能是用户手工加的。
            None => {
                let old = parse_legacy(&text);
                spec.vision |= old.vision;
                spec.image |= old.image;
                spec.tools.extend(old.tools);
                if !old.custom.is_empty() {
                    custom.push(old.custom);
                }
            }
        }
    }
    spec.custom = custom.join("\n\n");
    spec
}

/// 没有分段标记的老块：按小节标题认。
///
/// 这条路认得出「哪几项开着」，但认不出用户写在最后一节后面、又没有自己标题的
/// 那段文字 —— 它会被算进前一节的正文里。这是老格式本身的歧义，不是可以修好的
/// 东西；界面上那个「旧版」角标就是为它准备的：按一次「更新」重写成带标记的
/// 格式，之后就再也不会有这个问题。
fn parse_legacy(block: &str) -> InjectSpec {
    let mut spec = InjectSpec::default();
    let mut rest: Vec<&str> = Vec::new();
    let mut cursor = 0usize;
    for (start, len) in top_level_sections(block) {
        let head = block[cursor..start].trim();
        if !head.is_empty() {
            rest.push(head);
        }
        cursor = start + len;
        let section = &block[start..cursor];
        let first_line = section.lines().next().unwrap_or_default().trim_end();
        match first_line {
            VISION_HEADING => spec.vision = true,
            IMAGE_HEADING => spec.image = true,
            TOOLS_HEADING => spec.tools.extend(parse_tool_notes(section)),
            _ => rest.push(section.trim()),
        }
    }
    let tail = block[cursor..].trim();
    if !tail.is_empty() {
        rest.push(tail);
    }
    spec.custom = rest.join("\n\n");
    spec
}

/// 每个以 `## ` 开头的行到下一个这样的行之间算一节，返回 (起点, 长度)。
fn top_level_sections(block: &str) -> Vec<(usize, usize)> {
    let mut heads: Vec<usize> = Vec::new();
    let mut at = 0usize;
    for line in block.split_inclusive('\n') {
        if line.starts_with("## ") {
            heads.push(at);
        }
        at += line.len();
    }
    let end = block.len();
    heads
        .iter()
        .enumerate()
        .map(|(i, &s)| (s, heads.get(i + 1).copied().unwrap_or(end) - s))
        .collect()
}

fn parse_tool_notes(section: &str) -> Vec<ToolNote> {
    section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let body = line.strip_prefix("- `")?;
            let (name, tail) = body.split_once('`')?;
            let note = tail.split_once(TOOL_SEP)?.1.trim();
            (!name.trim().is_empty() && !note.is_empty()).then(|| ToolNote {
                name: name.trim().to_string(),
                note: note.to_string(),
            })
        })
        .collect()
}

/// 把 `block` 放进 `doc` 的标记之间；`None` 表示删除这一段。
///
/// **块外一个字节都不动**是这个函数的全部意义，改它之前先看测试。
fn replace_block(doc: &str, block: Option<&str>) -> String {
    let wrapped = block.map(|b| format!("{BEGIN}\n\n{b}\n\n{END}"));

    // 已有块：原地换掉。找不到 END 时按「没有块」处理 —— 半个标记多半是用户
    // 手工删了一半，此时贸然从 BEGIN 删到文件尾会吃掉他后面写的所有东西。
    if let Some(start) = doc.find(BEGIN) {
        if let Some(rel_end) = doc[start..].find(END) {
            let end = start + rel_end + END.len();
            let before = &doc[..start];
            let after = &doc[end..];
            return match wrapped {
                Some(w) => format!("{before}{w}{after}"),
                // 删除时把块两边多余的空行也收一收，免得反复装卸攒出一堆空行。
                None => {
                    let joined = format!("{}\n{}", before.trim_end(), after.trim_start());
                    let t = joined.trim();
                    if t.is_empty() {
                        String::new()
                    } else {
                        format!("{t}\n")
                    }
                }
            };
        }
    }

    // 没有块。删除是空操作；新增追加到文件末尾，不动用户原有内容的顺序。
    let Some(w) = wrapped else {
        return doc.to_string();
    };
    if doc.trim().is_empty() {
        return format!("{w}\n");
    }
    format!("{}\n\n{w}\n", doc.trim_end())
}

/// 读回一个 CLI 的注入状态。读不动的文件算「没注入」而不是报错 —— 这一格只是
/// 用来点亮界面的。
pub fn state(root: &ConfigRoot, target: CliTarget) -> InjectState {
    let rel = instructions_path(target);
    let path = root.join(rel);
    let doc = std::fs::read_to_string(&path).unwrap_or_default();
    let block = extract_block(&doc);
    let spec = block.as_deref().map(parse_block);
    // 解析出来的 spec 重新渲染一遍还原不成原样 = 块是旧版本写的。
    let outdated = match (&block, &spec) {
        (Some(b), Some(s)) => &render_block(s) != b,
        _ => false,
    };
    InjectState {
        target,
        label: target.label(),
        path: path.display().to_string(),
        exists: path.exists(),
        injected: block.is_some(),
        block,
        spec,
        outdated,
        chars: doc.chars().count(),
    }
}

/// 取出标记之间的内容。同样要求 BEGIN/END 成对，理由见 `replace_block`。
fn extract_block(doc: &str) -> Option<String> {
    let start = doc.find(BEGIN)? + BEGIN.len();
    let rel_end = doc[start..].find(END)?;
    Some(doc[start..start + rel_end].trim().to_string())
}

pub fn states(root: &ConfigRoot, targets: &[CliTarget]) -> Vec<InjectState> {
    targets.iter().copied().map(|t| state(root, t)).collect()
}

/// 写入（或移除）一个 CLI 的注入块，返回被写的文件路径。
///
/// 先快照后写：这些文件是用户攒出来的，必须能一键还原。走 `snapshot_extra`
/// 而不是 `snapshot`，因为它们不在 `CliTarget::relative_paths()` 里
/// （那份清单是「接管会改的配置」，指令文件不属于接管）。
pub fn apply(
    root: &ConfigRoot,
    target: CliTarget,
    spec: &InjectSpec,
    stamp: &str,
    backups: &BackupStore,
) -> Result<String, AppError> {
    let rel = instructions_path(target);
    let path = root.join(rel);
    // 文件在、却读不出来（权限、非 UTF-8）时必须报错停下。以前 unwrap_or_default
    // 把这种情况当成「空文件」，接着把整份 CLAUDE.md 替换成只剩我们那一块 ——
    // 有快照能回滚，但用户要等到 CLI 行为变了才发现。
    let doc = match std::fs::read_to_string(&path) {
        Ok(d) => d,
        Err(_) if !path.exists() => String::new(),
        Err(e) => {
            return Err(AppError::Io(format!(
                "读不出 {}：{e}。不会用一个只含注入块的文件把它盖掉",
                path.display()
            )))
        }
    };

    let rendered = render_block(spec);
    let block = if spec.is_empty() { None } else { Some(rendered.as_str()) };
    let next = replace_block(&doc, block);

    // 内容没变就别写：一次无谓的写入会占掉一份快照额度（上限 5 份），
    // 把用户真正需要的那份原始快照挤出去。
    if next == doc {
        return Ok(path.display().to_string());
    }

    backups.snapshot_extra(root, target, rel, stamp, "system-inject")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    write_atomic(&path, &next)?;
    Ok(path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sandbox() -> (tempfile::TempDir, ConfigRoot, BackupStore) {
        let dir = tempfile::tempdir().unwrap();
        let root = ConfigRoot::sandbox(dir.path().to_path_buf());
        let bk = BackupStore::new(dir.path().join("bk"));
        (dir, root, bk)
    }

    fn spec() -> InjectSpec {
        InjectSpec {
            vision: true,
            image: false,
            tools: vec![ToolNote {
                name: "codegraph".into(),
                note: "改代码前先查调用链，比 grep 准".into(),
            }],
            custom: "永远说中文。".into(),
        }
    }

    /// 只有名字没有说明的条目要跳过：光写个工具名对模型没有任何增量信息
    /// （工具清单它本来就看得到），白占每次请求的 token。
    #[test]
    fn tool_without_a_note_is_dropped() {
        let spec = InjectSpec {
            tools: vec![
                ToolNote { name: "has-note".into(), note: "用它做 X".into() },
                ToolNote { name: "no-note".into(), note: "   ".into() },
                ToolNote { name: "  ".into(), note: "没有名字".into() },
            ],
            ..Default::default()
        };
        let out = render_block(&spec);
        assert!(out.contains("has-note"), "{out}");
        assert!(!out.contains("no-note"), "{out}");
        assert!(!out.contains("没有名字"), "{out}");
    }

    /// 全是空说明时整个 spec 视为空 —— 不该因为勾了几个没写说明的工具就往
    /// 用户文件里塞一个只有标题的空块。
    #[test]
    fn tools_with_no_notes_do_not_make_a_block() {
        let spec = InjectSpec {
            tools: vec![ToolNote { name: "x".into(), note: "".into() }],
            ..Default::default()
        };
        assert!(spec.is_empty());
    }

    /// 这个模块的全部承诺：块外一个字节都不动。
    #[test]
    fn user_content_outside_the_block_survives_everything() {
        let (_keep, root, bk) = sandbox();
        let rel = instructions_path(CliTarget::ClaudeCode);
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mine = "# 我的规则\n\n攒了三个月的东西，一个字都不能丢。\n";
        std::fs::write(&path, mine).unwrap();

        apply(&root, CliTarget::ClaudeCode, &spec(), "s1", &bk).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(mine.trim_end()), "{after}");
        assert!(after.contains("describe_image"), "{after}");
        assert!(after.contains("永远说中文。"), "{after}");

        // 再写一次不该长出第二个块。
        apply(&root, CliTarget::ClaudeCode, &spec(), "s2", &bk).unwrap();
        let twice = std::fs::read_to_string(&path).unwrap();
        assert_eq!(twice.matches(BEGIN).count(), 1, "{twice}");

        // 移除后回到原样。
        apply(&root, CliTarget::ClaudeCode, &InjectSpec::default(), "s3", &bk).unwrap();
        let removed = std::fs::read_to_string(&path).unwrap();
        assert_eq!(removed.trim(), mine.trim(), "{removed}");
    }

    /// 装卸反复来回不该攒出一堆空行 —— 那是「原地替换」没做干净的典型症状。
    #[test]
    fn repeated_install_remove_does_not_grow_blank_lines() {
        let (_keep, root, bk) = sandbox();
        let path = root.join(instructions_path(CliTarget::Codex));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "原始内容\n").unwrap();

        for i in 0..4 {
            apply(&root, CliTarget::Codex, &spec(), &format!("a{i}"), &bk).unwrap();
            apply(&root, CliTarget::Codex, &InjectSpec::default(), &format!("b{i}"), &bk).unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "原始内容\n");
    }

    /// 用户手工删掉半个标记时，不能从 BEGIN 一路删到文件尾 —— 那会吃掉他后面
    /// 写的所有东西。找不到 END 就当没有块，追加一个新的。
    #[test]
    fn half_a_marker_does_not_eat_the_rest_of_the_file() {
        let doc = format!("{BEGIN}\n旧内容\n\n# 后面还有我的东西\n重要\n");
        let out = replace_block(&doc, Some("新内容"));
        assert!(out.contains("# 后面还有我的东西"), "{out}");
        assert!(out.contains("重要"), "{out}");
        assert!(out.contains("新内容"), "{out}");
    }

    /// 五家的路径都要能建出来（父目录可能还不存在）。
    #[test]
    fn writes_to_every_cli_creating_missing_dirs() {
        let (_keep, root, bk) = sandbox();
        for target in crate::services::cli_extensions::ALL_TARGETS {
            let written = apply(&root, target, &spec(), "s1", &bk).unwrap();
            let body = std::fs::read_to_string(&written)
                .unwrap_or_else(|e| panic!("{target:?}: {e}"));
            assert!(body.contains("describe_image"), "{target:?}");

            let st = state(&root, target);
            assert!(st.injected, "{target:?} 写完应当读得回来");
            assert!(st.block.unwrap().contains("read_image_text"), "{target:?}");
        }
    }

    /// 空 spec 写进一个从没存在过的文件时，不该凭空造出一个空文件。
    #[test]
    fn empty_spec_on_missing_file_creates_nothing() {
        let (_keep, root, bk) = sandbox();
        apply(&root, CliTarget::GeminiCli, &InjectSpec::default(), "s1", &bk).unwrap();
        assert!(!root.join(instructions_path(CliTarget::GeminiCli)).exists());
    }

    /// 端到端：完整 spec（视觉 + 第三方说明 + 自定义规则）写进**五家**，
    /// 逐个文件核对内容，再验往返、幂等、还原。
    ///
    /// 这条盯的是「填了说明 → 真的写进那五个 md」整条路。之前只验到「不报错」
    /// 和「含 describe_image」，第三方说明那一段是新加的，没人看着。
    #[test]
    fn full_spec_lands_in_every_cli_and_can_be_undone() {
        let (_keep, root, bk) = sandbox();
        let mine = "# 我自己的规则\n\n攒了很久的东西。\n";

        // 每家先放一份用户原有内容 —— 注入必须绕开它。
        for target in crate::services::cli_extensions::ALL_TARGETS {
            let path = root.join(instructions_path(target));
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, mine).unwrap();
        }

        let spec = InjectSpec {
            vision: true,
            image: true,
            tools: vec![
                ToolNote {
                    name: "codegraph".into(),
                    note: "改代码前先查调用链，比 grep 准".into(),
                },
                ToolNote {
                    name: "zread".into(),
                    note: "看不想克隆的 GitHub 仓库".into(),
                },
                // 没写说明的：不该出现在任何一家的文件里。
                ToolNote { name: "dune".into(), note: "  ".into() },
            ],
            custom: "永远用中文回答。".into(),
        };

        for target in crate::services::cli_extensions::ALL_TARGETS {
            let written = apply(&root, target, &spec, "e2e-1", &bk).unwrap();
            let body = std::fs::read_to_string(&written).unwrap();

            assert!(body.starts_with(mine.trim_end()), "{target:?} 用户内容被动了：{body}");
            assert_eq!(body.matches(BEGIN).count(), 1, "{target:?} 标记不唯一");
            assert_eq!(body.matches(END).count(), 1, "{target:?} 标记不唯一");

            // 四段都在
            assert!(body.contains("describe_image"), "{target:?} 缺视觉段");
            assert!(body.contains("read_image_text"), "{target:?} 缺视觉段");
            assert!(body.contains("generate_image"), "{target:?} 缺生图段");
            assert!(body.contains("edit_image"), "{target:?} 缺生图段");
            assert!(body.contains("`codegraph` —— 改代码前先查调用链，比 grep 准"), "{target:?} 缺第三方说明");
            assert!(body.contains("`zread` —— 看不想克隆的 GitHub 仓库"), "{target:?} 缺第三方说明");
            assert!(body.contains("永远用中文回答。"), "{target:?} 缺自定义规则");
            // 没说明的条目一个字都不该出现
            assert!(!body.contains("dune"), "{target:?} 把没说明的条目也写进去了：{body}");

            // 往返：读回来的块里有这些内容
            let st = state(&root, target);
            assert!(st.injected, "{target:?} 写完读不回来");
            let block = st.block.unwrap();
            assert!(block.contains("codegraph"), "{target:?} 块里没有第三方说明");
            assert!(block.contains("永远用中文回答。"), "{target:?} 块里没有自定义规则");
        }

        // 幂等：再写一遍内容完全一致，不会长出第二个块
        for target in crate::services::cli_extensions::ALL_TARGETS {
            let path = root.join(instructions_path(target));
            let before = std::fs::read_to_string(&path).unwrap();
            apply(&root, target, &spec, "e2e-2", &bk).unwrap();
            assert_eq!(before, std::fs::read_to_string(&path).unwrap(), "{target:?} 重复写入不幂等");
        }

        // 还原：清空 spec 之后回到用户原文
        for target in crate::services::cli_extensions::ALL_TARGETS {
            apply(&root, target, &InjectSpec::default(), "e2e-3", &bk).unwrap();
            let after = std::fs::read_to_string(root.join(instructions_path(target))).unwrap();
            assert_eq!(after.trim(), mine.trim(), "{target:?} 移除后没回到原样");
        }
    }

    /// 提示里的工具名必须和 MCP 真正暴露的一致，否则等于教模型调一个不存在的
    /// 工具 —— 比不写还糟。
    #[test]
    fn advertised_tool_names_match_the_mcp_server() {
        let text = vision_section();
        for name in [
            "describe_image",
            "read_image_text",
            "compare_images",
            "list_pasted_images",
            "describe_screen",
        ] {
            assert!(text.contains(name), "缺少 {name}");
        }
        assert!(text.contains(crate::services::vision_mcp::MCP_NAME));
        assert!(
            text.contains("[Image 1]"),
            "必须教模型用 image 编号，而不是去问用户要路径"
        );
        assert!(
            !text.contains("先问用户要"),
            "旧文案会让模型把「把图存到 Downloads」当成标准流程：{text}"
        );
    }

    /// 生图那一段同理：工具名对不上就是教模型调一个不存在的工具。
    #[test]
    fn image_section_names_match_the_image_mcp() {
        let text = image_section();
        for name in ["generate_image", "edit_image"] {
            assert!(text.contains(name), "缺少 {name}");
        }
        assert!(text.contains(crate::services::image_mcp::MCP_NAME));
        // 「结果是路径不是图」是这个 MCP 最反直觉的一点，必须写在提示里：
        // 不说的话模型会以为工具结果里有图，然后对着一行路径描述「画面内容」。
        assert!(
            text.contains("describe_image"),
            "得告诉模型怎么看自己画出来的东西：{text}"
        );
        assert!(
            text.contains("原图不动"),
            "改图另存新文件这件事要写明，否则模型会先备份一份再改"
        );
    }

    /// 两段互不影响：只开生图时不该冒出视觉那段（那段的开头是「你看不见图片」，
    /// 对一个多模态模型说这句话会让它开始拒绝看图）。
    #[test]
    fn sections_are_independent() {
        let only_image = render_block(&InjectSpec {
            image: true,
            ..Default::default()
        });
        assert!(only_image.contains("generate_image"));
        assert!(
            !only_image.contains("你**看不见图片**"),
            "只开生图却带出了视觉段：{only_image}"
        );

        let only_vision = render_block(&InjectSpec {
            vision: true,
            ..Default::default()
        });
        assert!(only_vision.contains("describe_image"));
        assert!(!only_vision.contains("generate_image"));

        // 都不开 = 空 spec，界面据此决定是「写入」还是「移除」。
        assert!(InjectSpec::default().is_empty());
        assert!(!InjectSpec { image: true, ..Default::default() }.is_empty());
    }

    // -----------------------------------------------------------------------
    // 把块拆回 spec
    //
    // 界面上的勾选框要从磁盘上的块回显。以前是在前端「渲染一遍再看包不包含」
    // ——只要我们自己改一个字，上一个版本写进用户文件的块就对不上了：勾选框
    // 显示成没勾，那段旧文字被当成用户手写内容，再点一次「更新」就写出一段旧
    // 的加一段新的。下面几条钉住新的做法。
    // -----------------------------------------------------------------------

    /// 渲染出去再读回来必须是同一个 spec，工具说明和用户那段一并还原。
    #[test]
    fn parse_round_trips_what_render_wrote() {
        let full = InjectSpec {
            vision: true,
            image: true,
            tools: vec![
                ToolNote { name: "codegraph".into(), note: "改代码前先查调用链".into() },
                ToolNote { name: "playwright".into(), note: "要点开页面看才算数".into() },
            ],
            custom: "永远说中文。\n\n提交前跑一遍测试。".into(),
        };
        assert_eq!(parse_block(&render_block(&full)), full);

        // 单项开关的组合同样要还原，别只在「全开」这一种形状下成立。
        for spec in [
            InjectSpec { vision: true, ..Default::default() },
            InjectSpec { image: true, ..Default::default() },
            InjectSpec { custom: "只有我自己写的".into(), ..Default::default() },
            InjectSpec::default(),
        ] {
            assert_eq!(parse_block(&render_block(&spec)), spec, "{spec:?}");
        }
    }

    /// **这条是那个 bug 的回归测试。** 下面这段是 v0.1.0-beta.20260821 之前的
    /// 视觉段原文（从真实的 `~/.claude/CLAUDE.md` 里取的），措辞和现在不一样。
    /// 它必须仍然算「视觉段开着」，而不是掉进 custom 里。
    #[test]
    fn an_older_wording_still_counts_as_the_same_section() {
        let old_block = "\
## 图片处理（ccLoad 视觉辅助）

你**看不见图片**。本机装了 MCP 服务器 `ccload-vision`，它把图片交给一个多模态模型
再把文字结果给你。凡是遇到图片，必须调用下面的工具：

- `describe_image` —— 看懂一张图。参数 `path`（绝对路径）或 `url` 二选一。
- `read_image_text` —— 逐字抄下图上的文字。

用户贴来的图片通常是本地文件路径。拿不到路径时先问用户要，不要跳过。";

        let got = parse_block(old_block);
        assert!(got.vision, "旧措辞被判成没勾视觉");
        assert!(!got.image);
        assert_eq!(got.custom, "", "旧的生成内容不能被当成用户手写的留下来");
    }

    /// 认不出来的小节要原样留给用户 —— 宁可把我们生成的东西错当成用户的，
    /// 也不能反过来把用户手写的规则吃掉。
    #[test]
    fn unknown_sections_stay_with_the_user() {
        let block = "\
## 图片处理（ccLoad 视觉辅助）

（正文略）

## 我自己的规范

- 一律 rebase，不 merge
- 提交信息写中文

末尾还有一句。";
        let got = parse_block(block);
        assert!(got.vision);
        assert!(
            got.custom.starts_with("## 我自己的规范"),
            "用户那节丢了：{:?}",
            got.custom
        );
        assert!(got.custom.contains("一律 rebase"));
        assert!(got.custom.ends_with("末尾还有一句。"));
        assert!(!got.custom.contains("正文略"), "视觉段的正文不该混进来");
    }

    /// 工具说明按第一个 `——` 切：说明本身再出现一次不能被截断。
    #[test]
    fn a_note_may_contain_the_separator() {
        let spec = InjectSpec {
            tools: vec![ToolNote {
                name: "zread".into(),
                note: "看别人的仓库 —— 不要 clone 下来再翻".into(),
            }],
            ..Default::default()
        };
        assert_eq!(parse_block(&render_block(&spec)), spec);
    }

    /// 装的是旧版本的措辞时要能看出来，界面才有理由提示「按一下更新」。
    #[test]
    fn outdated_is_flagged_so_the_ui_can_offer_a_refresh() {
        let (_keep, root, bk) = sandbox();
        let rel = instructions_path(CliTarget::ClaudeCode);
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("{BEGIN}\n\n## 图片处理（ccLoad 视觉辅助）\n\n旧版本的正文。\n\n{END}\n"),
        )
        .unwrap();

        let st = state(&root, CliTarget::ClaudeCode);
        assert!(st.injected);
        assert!(st.spec.as_ref().unwrap().vision, "旧块也要认出视觉段");
        assert!(st.outdated, "措辞变了却没标成旧版");

        // 按「更新」重写一遍之后就不该再报旧版了。
        apply(
            &root,
            CliTarget::ClaudeCode,
            &InjectSpec { vision: true, ..Default::default() },
            "s1",
            &bk,
        )
        .unwrap();
        let after = state(&root, CliTarget::ClaudeCode);
        assert!(!after.outdated, "重写之后还在报旧版");
        assert_eq!(after.spec.unwrap(), InjectSpec { vision: true, ..Default::default() });
    }

    /// 没注入的文件不该被标成「旧版」—— 那一格只在装着东西时才有意义。
    #[test]
    fn a_file_without_a_block_is_not_outdated() {
        let (_keep, root, _bk) = sandbox();
        let st = state(&root, CliTarget::GeminiCli);
        assert!(!st.injected);
        assert!(!st.outdated);
        assert!(st.spec.is_none());
    }

    /// 分段标记存在的意义：用户那段写在最后、又没有自己的标题时，边界只能靠它。
    /// 这一条如果红了，说明有人把 `MARK_CUSTOM` 拿掉了 —— 后果是那段文字回显不
    /// 出来，再按一次「更新」就从磁盘上没了。
    #[test]
    fn text_after_the_last_section_is_not_swallowed() {
        let spec = InjectSpec {
            vision: true,
            image: true,
            custom: "我自己的规则，没有标题，就跟在最后一节后面。".into(),
            ..Default::default()
        };
        let block = render_block(&spec);
        assert_eq!(parse_block(&block).custom, spec.custom);

        // 同样的内容，去掉标记（= 老格式）就再也分不出来了 —— 这是老格式本身的
        // 歧义，也正是「旧版」角标存在的理由。
        let legacy: String = block
            .lines()
            .filter(|l| marker_kind(l).is_none())
            .collect::<Vec<_>>()
            .join("\n");
        let got = parse_block(&legacy);
        assert!(got.vision && got.image, "开关还是要认得出来");
        assert!(
            state_is_outdated(&legacy),
            "认不全的老块必须报旧版，用户才知道按「更新」"
        );
    }

    fn state_is_outdated(block: &str) -> bool {
        render_block(&parse_block(block)) != block
    }
}


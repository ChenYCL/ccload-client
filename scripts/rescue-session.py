#!/usr/bin/env python3
"""把撑爆上游窗口、连 /compact 都发不出去的 Claude Code 会话瘦回能用的大小。

## 为什么会卡死

Claude Code 按**模型声明的窗口**算压缩时机，而走 ccLoad 时真正拦你的是**中转
那一家的上限**。两个数不一样的时候（典型：模型名挂了 `[1m]`，中转其实只给
500k），压缩阈值就被算在一个不存在的分母上，等触发时已经越过真实天花板了。

越过之后是死锁：`/compact` 自己也是一次请求，也要把整段 transcript 发上去，
所以它同样超限。会话再也发不出任何东西，只会一直：

    400 {"code":"invalid-argument","error":"This model's maximum prompt
         length is 500000 but the request contains 517306 tokens."}

## 救法

会话在磁盘上是一份 JSONL（`~/.claude/projects/<slug>/<session-id>.jsonl`），
`claude --resume` 会重新读它来重建上下文。所以把里面最占地方的东西换成占位符，
上下文就掉回天花板以下，会话活过来。

**只改内容，不删条目**：JSONL 是靠 `uuid`/`parentUuid` 串起来的链表，删行会
把链断开，恢复出来的会话缺胳膊少腿。所以就地改写 payload，条目和它的 uuid 原样
留着。

优先砍图片：一张 2000×1288 的截图约 3.4k tokens，几十张就是十几万，而三天前的
某张界面截图对当下的对话基本没有价值。砍完还不够才动超长的文本工具结果，且保留
首尾——中间那几百行 grep 输出可以丢，"这条命令跑了什么、结论是什么"不能丢。

## token 数从哪来

不靠估。每条 assistant 记录里有上游回报的真实 usage：

    input_tokens + cache_read_input_tokens + cache_creation_input_tokens

这就是那一轮实际发上去的上下文大小，也是和 400 报错里那个数字对齐的口径。字符
估算只用来**排序该砍谁**，不用来报数——按字符估会把每行的元数据、
`toolUseResult` 本地副本、以及根本不进上下文的 file-history 记录都算进去，实测
比真值高一倍多，照着它砍会砍过头。

## 一个前提

只对**从没成功压缩过**的会话有效。如果会话里已经有一次成功的 compact，
`--resume` 是从那个边界之后重建的，边界以前的条目本来就不进上下文，改它们
不会让上下文变小。脚本会检测并告诉你。

## 用法

    python3 scripts/rescue-session.py <session.jsonl>              # 只看报告
    python3 scripts/rescue-session.py <session.jsonl> --write      # 真的改
    python3 scripts/rescue-session.py <session.jsonl> --write \\
        --target 300000        # 瘦到这个 token 数以下就停手

改之前会把原文件另存一份 `.bak-<时间戳>`，`--write` 前必须先退出那个会话窗口
（进程里还持有内存态，你改完它一保存就又盖回去）。
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import time

# 一张图的 token 数 ≈ 像素数 / 750，Anthropic 文档口径。
PIXELS_PER_TOKEN = 750
# 拿不到尺寸时的兜底。base64 长度和像素数不是线性关系（PNG 压缩率差很多），
# 与其算错不如给个保守常数——它只参与排序。
FALLBACK_IMAGE_TOKENS = 1500
# 文本按字符估 token。中英文混排 + 代码，3.5 是实测比较接近的系数。
CHARS_PER_TOKEN = 3.5


def est_text_tokens(s: str) -> int:
    return int(len(s) / CHARS_PER_TOKEN)


def walk(node, path=()):
    """遍历出每一个 (容器, 键, 值)，这样才能就地改写而不用重建整棵树。"""
    if isinstance(node, dict):
        for k, v in list(node.items()):
            yield node, k, v, path + (k,)
            yield from walk(v, path + (k,))
    elif isinstance(node, list):
        for i, v in enumerate(node):
            yield node, i, v, path + (i,)
            yield from walk(v, path + (i,))


def is_b64_image(parent, key, val) -> bool:
    """确认这是一段 base64 图片，而不是碰巧叫 data 的长字符串。"""
    if not isinstance(val, str) or len(val) < 4096 or key not in ("data", "base64"):
        return False
    if not isinstance(parent, dict):
        return False
    sibling = " ".join(str(v) for k, v in parent.items() if k != key)
    return "image/" in sibling or parent.get("type") == "base64"


def image_tokens(entry) -> int:
    """这一条里的图片值多少 token。有尺寸就按尺寸算，没有就用兜底常数。"""
    dims = None
    for _, key, val, _ in walk(entry):
        if key == "dimensions" and isinstance(val, dict):
            w = val.get("originalWidth") or val.get("displayWidth")
            h = val.get("originalHeight") or val.get("displayHeight")
            if w and h:
                dims = int(w) * int(h) // PIXELS_PER_TOKEN
            break
    n = sum(1 for p, k, v, _ in walk(entry) if is_b64_image(p, k, v))
    if not n:
        return 0
    # message 里和 toolUseResult 里各存一份同一张图，只有前者进上下文。
    return dims or FALLBACK_IMAGE_TOKENS


def context_weight(entry) -> int:
    """这一条进上下文时大约值多少 token —— **只用来排序**，不用来报数。

    只看 `message`：那才是发给模型的东西。`toolUseResult` 是 Claude Code 留的
    本地副本，元数据（cwd/sessionId/version/gitBranch）每行都有一份，
    file-history-snapshot 之类根本不进上下文——全算进来会高出一倍多。
    """
    if entry.get("type") not in ("user", "assistant"):
        return 0
    msg = entry.get("message")
    if not isinstance(msg, (dict, list)):
        return 0
    total = image_tokens(entry)
    for parent, key, val, _ in walk(msg):
        if isinstance(val, str) and not is_b64_image(parent, key, val):
            total += est_text_tokens(val)
    return total


def real_context(entries):
    """从上游回报的 usage 里取真实上下文大小。返回 (最后一轮, 峰值)。"""
    last = peak = 0
    for e in entries:
        if e.get("type") != "assistant":
            continue
        u = (e.get("message") or {}).get("usage")
        if not isinstance(u, dict):
            continue
        n = sum(int(u.get(k) or 0) for k in
                ("input_tokens", "cache_read_input_tokens", "cache_creation_input_tokens"))
        if n:
            last = n
            peak = max(peak, n)
    return last, peak


def has_compact_boundary(entries) -> bool:
    """会话里是否已经有一次成功的压缩。有的话边界以前的条目不进上下文。"""
    for e in entries:
        if e.get("isCompactSummary") or e.get("subtype") == "compact_boundary":
            return True
        if e.get("type") == "system" and "compact" in str(e.get("content", "")).lower():
            return True
    return False


def strip_images(entry) -> int:
    """把图片换成一句占位符。返回省下的 token（按 context_weight 口径）。"""
    saved = image_tokens(entry)
    if not saved:
        return 0
    for parent, key, val, _ in walk(entry):
        if not is_b64_image(parent, key, val):
            continue
        parent[key] = ""
        # 改成文本块，模型看到"这里原本有张图"，而不是一个来路不明的空字段。
        if parent.get("type") == "base64":
            parent.clear()
            parent["type"] = "text"
            parent["text"] = "[图片已被 rescue-session 移除以腾出上下文]"
    return saved


def truncate_text(entry, limit_chars: int) -> int:
    """超长文本留首尾，中间换成一行说明。只动 `message`（进上下文的那份）。"""
    msg = entry.get("message")
    if not isinstance(msg, (dict, list)):
        return 0
    saved = 0
    head, tail = limit_chars // 2, limit_chars - limit_chars // 2
    for parent, key, val, _ in walk(msg):
        if not isinstance(val, str) or len(val) <= limit_chars:
            continue
        if is_b64_image(parent, key, val):
            continue  # 图片上一步已处理，再截会留下半张图的垃圾
        before = est_text_tokens(val)
        cut = len(val) - limit_chars
        parent[key] = (
            val[:head]
            + f"\n\n… [rescue-session 截掉中间 {cut} 字符以腾出上下文] …\n\n"
            + val[-tail:]
        )
        saved += before - est_text_tokens(parent[key])
    return saved


def session_is_live(path: str):
    """会话正被某个 claude 进程持有就返回原因，否则 None。

    改一个活着的会话是白改：进程里有内存态，下一次落盘会把你的修改盖掉。
    """
    sid = os.path.basename(path).removesuffix(".jsonl")
    try:
        ps = subprocess.run(["ps", "-eo", "pid,command"],
                            capture_output=True, text=True, timeout=10).stdout
    except Exception:
        ps = ""
    for line in ps.splitlines():
        if "share/claude/versions" not in line and "ClaudeCode.app" not in line:
            continue
        if sid in line or path in line:
            return f"PID {line.strip().split(None, 1)[0]} 正在用这个会话"
    age = time.time() - os.path.getmtime(path)
    if age < 120:
        return f"文件 {int(age)} 秒前还在被写（可能有窗口开着）"
    return None


def main():
    ap = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("session", help="会话 JSONL 路径")
    ap.add_argument("--write", action="store_true", help="真的改；不加就只出报告")
    ap.add_argument("--target", type=int, default=300_000,
                    help="瘦到这个真实 token 数以下（默认 300000）")
    ap.add_argument("--text-limit", type=int, default=4000,
                    help="单条文本超过多少字符就截（默认 4000）")
    ap.add_argument("--force", action="store_true", help="会话看起来还活着也照改（不建议）")
    args = ap.parse_args()

    path = os.path.abspath(os.path.expanduser(args.session))
    if not os.path.isfile(path):
        sys.exit(f"找不到：{path}")

    live = session_is_live(path)
    if live and args.write and not args.force:
        sys.exit(f"拒绝改：{live}。先退出那个 Claude Code 窗口，或加 --force。")
    if live:
        print(f"注意：{live}\n")

    entries = []
    with open(path, encoding="utf-8") as fh:
        for i, line in enumerate(fh):
            line = line.strip()
            if not line:
                continue
            try:
                entries.append(json.loads(line))
            except json.JSONDecodeError as e:
                sys.exit(f"第 {i + 1} 行不是合法 JSON（{e}）—— 文件可能已损坏，先恢复备份")

    last, peak = real_context(entries)
    weight = sum(context_weight(e) for e in entries)
    print(f"{len(entries)} 条记录")
    print(f"真实上下文（上游回报）：最后一轮 {last:,}，峰值 {peak:,}")
    if not last:
        sys.exit("这份 transcript 里没有 usage 记录，拿不到真实上下文——不敢下手。")

    if has_compact_boundary(entries):
        print("\n注意：这个会话已经成功压缩过。边界以前的条目本来就不进上下文，"
              "\n      砍它们不会让上下文变小。确认它真的卡在超限再继续。")

    if last <= args.target:
        print(f"\n最后一轮已经在目标 {args.target:,} 以下，不用动。")
        if not args.write:
            return

    # 真实值 / 字符估算 = 换算系数。用它把"砍掉多少估算 token"折算回真实 token，
    # 免得按估算砍完发现真实值只掉了一半。
    scale = last / weight if weight else 1.0
    need_est = int((last - args.target) / scale) if scale else 0
    print(f"\n估算权重 {weight:,}（换算系数 {scale:.2f}），需要砍掉约 {need_est:,} 估算 token")

    order = sorted(range(len(entries)), key=lambda i: -context_weight(entries[i]))
    cut = 0
    n_img = n_txt = 0
    for i in order:
        if cut >= need_est:
            break
        s = strip_images(entries[i])
        if s:
            n_img += 1
            cut += s
        if cut >= need_est:
            break
        s = truncate_text(entries[i], args.text_limit)
        if s:
            n_txt += 1
            cut += s

    projected = int(last - cut * scale)
    print(f"改了 {n_img} 条的图片、{n_txt} 条的长文本")
    print(f"预计上下文 {last:,} → 约 {projected:,}")
    if projected > args.target:
        print(f"警告：仍高于目标 {projected - args.target:,} —— 把 --text-limit 调小再来一次")

    if not args.write:
        print("\n这是预演。确认没问题就加 --write。")
        return

    bak = f"{path}.bak-{time.strftime('%Y%m%d-%H%M%S')}"
    shutil.copy2(path, bak)
    tmp = path + ".tmp"
    with open(tmp, "w", encoding="utf-8") as fh:
        for e in entries:
            fh.write(json.dumps(e, ensure_ascii=False) + "\n")
        fh.flush()
        os.fsync(fh.fileno())
    shutil.copystat(path, tmp)
    os.replace(tmp, path)
    print(f"\n已写入。备份：{bak}")
    print(f"恢复会话：claude --resume {os.path.basename(path).removesuffix('.jsonl')}")


if __name__ == "__main__":
    main()

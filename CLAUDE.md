# CLAUDE.md

本仓库的开发须知统一写在 [`AGENTS.md`](./AGENTS.md) 里 —— 一份内容，Claude Code、
Codex、OpenCode、Gemini CLI 和人类开发者共用，不维护两份会各自漂移的副本。

**动手之前请先读 `AGENTS.md`。** 里面有几条不读就一定会踩的硬约束：

1. `vendor/ccLoad` 是上游内核，**只读**；换内核版本改 `KERNEL_VERSION`。
2. 内核已经能做的事（代理、故障转移、协议转换、拉上游模型清单）不要在客户端重做。
3. 写用户 CLI 配置必须：原子写 + 保留原权限 + 先快照 + 按键合并（不整块替换）。
4. macOS 包必须是 universal，否则 Apple Silicon 上白屏闪退。

提交前三条命令要全绿：

```bash
pnpm typecheck
cd src-tauri && cargo clippy --all-targets -- -D warnings
cd src-tauri && cargo test
```

<p align="center">
  <img src="docs/assets/logo.png" width="88" height="88" alt="ccLoad" />
</p>

# ccLoad Desktop

**Desktop client for ccLoad — for Claude Code, Codex, Gemini, Grok Build, and OpenCode.**

**English | [简体中文](./README.zh-CN.md)**

> Point every CLI at one gateway | Local or remote | Keep your MCP | Undo a takeover | Try in a sandbox first

<p align="center">
  <img src="docs/assets/hero.png" alt="ccLoad Desktop — one kernel, every CLI" />
</p>

ccLoad already removes the operational mess of multiple AI API upstreams: routing, failover, protocol conversion, usage. What is still messy is the laptop — five coding CLIs, five “send requests here” settings. Changing gateways means pasting an address five times. Many switchers rewrite the whole file and take your MCP servers and current model with them.

ccLoad Desktop is the window that closes that gap. Start a local [ccLoad](https://github.com/caidaoli/ccLoad), or connect to one you already run, then point the CLIs you actually use at it. You keep working in the same terminals. Channels and keys stay in ccLoad.

## Who, what, why, when, where, how

| | |
|---|---|
| **Who** | Anyone already using Claude Code, Codex, Gemini CLI, Grok Build, or OpenCode, who runs (or is about to run) ccLoad as the gateway. |
| **What** | A desktop switchboard. Not another proxy — the app that connects that gateway to those five CLIs. |
| **Why** | Routing, cooldown, and billing already live in ccLoad. On the machine you still edit five configs by hand, miss one, and watch a session hit the old endpoint; typical switchers overwrite the whole file. |
| **When** | A new CLI, a new gateway, a friend’s instance, a suspicion that one tool is still on the old URL, or a trial that must not touch the live session. |
| **Where** | The gateway can be this computer, a box at home, or a shared server. The CLIs stay in the terminals you already use. Installers for Windows, macOS, and Linux; one Mac build covers Intel and Apple Silicon. |
| **How** | Open the app → start local ccLoad or paste an existing URL → takeover the CLIs you use → keep using those CLIs as before → come back here for usage, health, and logs. |

<p align="center">
  <img src="docs/assets/flow.png" alt="Five CLIs → desktop app → your gateway" />
</p>

## What it solves

If you keep several AI CLIs open, these are the failure modes:

- **The URL has to be pasted five times.** Each tool has its own endpoint. Miss one when you change gateways, and that session dies mid-turn on the old address.
- **A switcher wipes the room.** Whole-file replace is how MCP servers vanish and the model you were using gets swapped out.
- **The wrong CLI has no undo.** Without a snapshot you are left with memory, or a full-disk backup.
- **You want a trial without burning the live session.** Comparing two gateways should not use the real `~/.claude` as the lab.
- **The gateway is up, the laptop is blind.** Usage, in-flight requests, and channel health still mean opening a browser and guessing.

ccLoad Desktop handles those cases with:

- **One takeover, only the CLIs you use.** You do not open five config files.
- **Add what is needed, never replace the file.** MCP, custom models, and fields you typed stay. Import appends to the catalog; it does not change the model you have selected.
- **A snapshot before every write.** Roll back that CLI only. The first copy is never thrown away.
- **A sandbox.** Writes land in a side folder until you turn sandbox off.
- **An overview that matches the gateway.** Spend, health, live requests, logs — the same ccLoad numbers, not a second counter.

## What you can do

| You want to | Open | You get |
|---|---|---|
| Run a gateway here, or join one you already have | Settings | This Mac as the entry, or the box you already share |
| Point CLIs at it | CLI takeover | Claude Code / Codex / Gemini / Grok / OpenCode on one gateway |
| Edit channels, keys, tokens | Kernel admin | The real ccLoad screens, not a thinner copy |
| Opus on this provider, fallback on that one | Dispatch graph | Conflicts are errors, not a silent pick |
| Put channel models into the CLI lists | Model import | Names are appended; a Claude Code row with no slot is skipped |
| See which MCP / Skill is on which CLI | Extensions | One row, badges for each tool |
| See today’s spend and a sick channel | Overview / live logs | The same figures as the ccLoad dashboard |

## Get it

[Releases](https://github.com/ChenYCL/ccload-client/releases)

| System | Download |
|---|---|
| macOS | `.dmg` / `.zip` (Intel and Apple Silicon in one file) |
| Windows | `.exe` |
| Linux | `.AppImage` or `.deb` |

macOS builds are unsigned for now: first launch is right-click the app → Open.

Turn on “sandbox CLI writes” while developing. Building from source and contributor rules: [AGENTS.md](./AGENTS.md).

## License

MIT — [LICENSE](./LICENSE). The bundled [ccLoad](https://github.com/caidaoli/ccLoad) kernel is MIT as well.

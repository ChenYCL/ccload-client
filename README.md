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

## Each sidebar page

The sidebar is three groups: watch what is happening, then change how it runs, then touch the environment. Pages follow that order. Screenshots match the shipped light shell (sidebar groups, kernel status in the corner). Numbers are sample data.

### Monitor · Overview

This is the first screen: how many requests today, how many succeeded, how busy the last minute was, how much it cost. Below that: the traffic curve, per-channel health, which models are spending. Figures come from the kernel; the client only aggregates.

![Overview](docs/assets/ui/page-dashboard.png)

### Monitor · Live logs

The top list is in-flight requests (not in history yet). The table is finished ones. Filter by model, channel, or status, or show errors only. Turn “live” off if you do not want the laptop polling the kernel; refresh by hand instead.

![Live logs](docs/assets/ui/page-logs.png)

### Configure · CLI takeover

Point Claude Code, Codex, Gemini CLI, Grok Build, and OpenCode at the current gateway. Already-pointed tools can be written again (to fix a half-edited file). Every write is snapshotted; “Snapshot history” in the corner rolls one CLI back. With sandbox on in Settings, this page only writes a side folder.

![CLI takeover](docs/assets/ui/page-cli.png)

### Configure · Dispatch graph

A table for “this kind of work uses that provider’s that model.” After you apply it, CLIs only speak tier aliases (opus / sonnet / …). Failover and cooldown stay in the kernel. Contradictory orderings are blocked instead of silently picked.

![Dispatch graph](docs/assets/ui/page-graph.png)

### Configure · Model chain

A fallback: try this, then that. Written as channels in decreasing priority so the kernel’s picker walks the whole chain. The graph is “how traffic is split”; the chain is “where it goes when something dies.”

![Model chain](docs/assets/ui/page-fallback.png)

### Configure · Model import

Aliases your live channels can actually serve, appended to Codex / OpenCode catalogs. Claude Code has no catalog file, only a few slots: a row is written only if you pick opus / sonnet / … — otherwise it is skipped, so the model you are using is not overwritten.

![Model import](docs/assets/ui/page-models.png)

### Configure · Extensions

One row per MCP / Skill / Agent / Hook, with badges for which CLIs have it. Edit once, push to every CLI that has it. File formats stay native; writes are snapshotted first.

![Extensions](docs/assets/ui/page-extensions.png)

### System · Kernel admin

Channels, tokens, the kernel’s own logs and settings. This opens ccLoad’s stock admin in a separate window, so fields follow kernel upgrades. There is no thinner copy of those forms here.

![Kernel admin](docs/assets/ui/page-web-admin.png)

### System · Settings

Where the gateway lives: run one on this machine, or paste an instance you already have. Turn on sandbox CLI writes while you experiment. The page also lists Anthropic / OpenAI / Gemini entrypoints for tools that do not go through takeover.

![Settings](docs/assets/ui/page-settings.png)

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

# Kareenos fork of Terminator — the Kareenos Desktop Agent

This repository is Kareenos' fork of [mediar-ai/terminator](https://github.com/mediar-ai/terminator)
(MIT). Upstream is kept as the `upstream` remote; our work lives on the `kareenos` branch.
Everything upstream stays as it is, so rebases stay cheap. What we add:

| Path | What |
|---|---|
| `crates/kareenos-desktop-agent/` | **The Kareenos Desktop Agent**: the Windows half of the Kareenos Desktop Channel (platform architecture §5.134). A console binary today; the tray app (Tauri) wraps the same library. |
| `.github/workflows/kareenos-desktop-agent.yml` | Builds + tests the agent on `windows-latest`, uploads the x64 exe; a manual Notepad spike job. |
| `crates/terminator-mcp-agent` | One change: PostHog analytics sit behind the `posthog` cargo feature (on by default, so upstream behaviour is unchanged). The Desktop Agent does not link that crate at all. |

## What the Desktop Agent is

The twin of the Kareenos browser extension (our mcp-chrome fork), for a Windows computer:

- one **outbound** `wss://…/desktop-channel` connection to the Kareenos browser tier, bound with a
  durable sliding token; nothing listens on the PC, ever;
- the platform is the **sole command source**; the agent runs the bounded thirteen actions
  (`snapshot apps open focus read screenshot scroll wait click fill press run_steps lane_close`)
  through the Terminator core (Windows UI Automation) and answers results;
- one queue per computer (one keyboard), acks and deadlines, `HUMAN_ACTIVE` when the person is
  using the computer, `PAUSED_BY_USER` from the tray;
- no shell, no files, no browser scripting, no telemetry.

The frozen contract is `docs/platform-design/desktop-channel-envelope.md` in the Kareenos docs repo;
`src/envelope.rs` mirrors it and the backend's selfcheck compares the two tables.

## Build

```
cargo build -p kareenos-desktop-agent --release        # Windows only (UIA)
cargo test  -p kareenos-desktop-agent
target\release\kareenos-desktop-agent.exe --selfcheck   # offline self-test
```

There is no Rust toolchain on the Kareenos Mac; CI (`kareenos-desktop-agent.yml`) is the build.
Push to `kareenos` and read the run.

## Run

```
kareenos-desktop-agent.exe "kareenos-desktop://connect?p=…"   # sign in once (link or code from Kareenos → Browsers → Connect this computer)
kareenos-desktop-agent.exe                                    # afterwards: run
kareenos-desktop-agent.exe --status | --sign-out | --paused | --take-over | --minutes N | --server wss://…
```

State: the sign-in is in the Windows Credential Manager ("Kareenos Desktop Agent"); install id,
logs and the pause flag live under `%LOCALAPPDATA%\Kareenos\DesktopAgent`; the optional local
policy is `%ProgramData%\Kareenos\DesktopAgent\policy.json` (deny / allow applications, idle window,
wss URL; local wins).

## Rules for this fork

- Never run `terminator release`, `npx @mediar-ai/cli release` or push `v*` tags: those are
  upstream's publishing paths (npm, crates.io, PyPI) and their workflows would try to run here.
- Do not edit upstream crates except where this file lists it. Add Kareenos code under
  `crates/kareenos-*` only.
- The agent links the `terminator` core crate only — never `terminator-mcp-agent` (Sentry,
  OpenTelemetry, PostHog, the local extension bridge on port 17373 live there).
- Rebase: `git fetch upstream && git rebase upstream/main` on `kareenos`, then rebuild.

## Release (Phase 2, Tauri tray app)

`scripts/build-desktop-agent-release.sh` will produce the signed MSI + `latest.json` and publish them
to the three trees the extension uses (`releases/desktop-agent/latest/` in this repo, the Kareenos
frontend `public/releases/`, the expressserver public folder) — see the plan in the Kareenos docs.

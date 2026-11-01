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

## The tray app (`apps/desktop-agent`, Phase 2)

A Tauri 2 shell around the library — nothing in it talks to the platform or to UI Automation
directly:

- `src-tauri/src/agent.rs` — the supervisor. Loads the stored sign-in, creates the UIA desktop
  once, runs `client::run` on its own thread + tokio runtime and turns the library's hooks
  (`ClientConfig.on_state`, `on_activity`) into one `Status` the UI renders. Sign in / sign out /
  "Use this computer" / quit are messages to it.
- `src-tauri/src/main.rs` — tray icon and menu (status line · Sign in… · Use this computer ·
  Pause agents · Diagnostics… · Check for updates · Help · Quit), the three windows, the
  `kareenos-desktop://` deep link (single-instance + deep-link plugins; `register_all()` at start
  as a belt and braces to the installer's registry entries), start-with-Windows (autostart
  plugin, enabled on sign-in, disabled on sign-out), the activity overlay (click-through pill,
  top-right, shown while a command runs, never showing a `fill` value).
- `src-tauri/src/trail.rs` — the last 80 log lines for Diagnostics / "Copy diagnostics".
- `ui/` — plain HTML/CSS/JS on the global Tauri API (no Node build step): `index.html`
  (welcome / sign-in / connected), `diagnostics.html`, `overlay.html`. Brand tokens are a local
  copy in `app.css`, like the extension's popup.
- The pause switch is the flag file in `%LOCALAPPDATA%\Kareenos\DesktopAgent` (shared with the
  console binary); the client's watcher sends the `state` frame.

Build on Windows: `npm i -g @tauri-apps/cli@^2` then `cd apps/desktop-agent && tauri build`
(the WiX toolset is downloaded by the CLI; output `target/release/bundle/msi/*.msi`). CI does
exactly this in the `app` job of `.github/workflows/kareenos-desktop-agent.yml` and uploads
`kareenos-desktop-agent-msi-win32-x64`. The job also proves the app opens no listening port.

Updater: compiled in only with `--features updater` (the plugin needs a public key in the
config). Release flow: `tauri signer generate -w <gitignored>/kareenos-desktop-agent.key` once;
build with `tauri build --features updater --config '{"plugins":{"updater":{"pubkey":"<pub>",
"endpoints":["https://kareenos.com/releases/desktop-agent/latest/latest.json"]}},"bundle":
{"createUpdaterArtifacts":true}}'` with `TAURI_SIGNING_PRIVATE_KEY` set; the `.sig` goes to the
release script. The app applies an update only when no command is in flight and the agent has
been idle 45 s (the 1.3.3 rule).

Not done yet: code signing (Azure Trusted Signing, decision 10 — SmartScreen will warn until
then), a CSP for the pages (null today; they are local files with no remote content), the
`AGENT_OUTDATED` bind answer on the server side.

## Release

`scripts/build-desktop-agent-release.sh --msi <file.msi> --version <x.y.z> [--sig <file.sig>]
[--notes "…"]` takes the MSI built by CI (download the artifact) and publishes ONE unversioned
file, `KareenosDesktopAgent-Setup-x64.msi`, plus `.sha256`, `latest.json` (Tauri updater manifest)
and `VERSION` to the three trees the extension uses: `releases/desktop-agent/latest/` in this
repo, `kareenos_frontend/public/releases/desktop-agent/latest/` (Quasar copies `public/`
verbatim) and `expressserver/public/kareenos_com/releases/desktop-agent/latest/`. Live at
`https://kareenos.com/releases/desktop-agent/latest/KareenosDesktopAgent-Setup-x64.msi` after the
usual frontend publish and expressserver deploy; then author the Downloads row in Super Admin ›
Downloads.

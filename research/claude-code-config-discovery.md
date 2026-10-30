# How Claude Code discovers config, skills, MCP servers and credentials (2026-10-30)

Answers [#3](https://github.com/jonaswre/boxlite/issues/3) on the [boxq map](https://github.com/jonaswre/boxlite/issues/1). This is a throwaway research branch and is never merged.

Sources:
- Official docs at `code.claude.com/docs/en/*` (cited by page name below, e.g. *settings*).
- The CLIProxyAPI source at commit `90c447d`.
- The installed Claude Code v2.1.312 binary. Claims marked **[binary]** come from string inspection of it.
- Live probes of v2.1.312 in tmux with a throwaway `CLAUDE_CONFIG_DIR`, a dummy key and an unreachable base URL. Claims marked **[probe]** come from those.

No secret values from this machine were read or recorded. Only file names and JSON key names were inspected.

## TL;DR for boxq

- Copy into the Sandbox:
  - `~/.claude/{CLAUDE.md, rules/, skills/, agents/, commands/, output-styles/}`. Dereference symlinks when copying.
  - A **filtered** `~/.claude/settings.json`.
  - The `mcpServers` subtrees of `~/.claude.json`.
- Never copy:
  - `~/.claude/.credentials.json`.
  - The rest of `~/.claude.json`. It holds `oauthAccount`, `userID`, `machineID`, caches, and possibly `primaryApiKey`.
  - Any session or state directory.
- Auth: CLIProxyAPI accepts the key in `Authorization: Bearer`, `X-Api-Key`, `X-Goog-Api-Key`, or `?key=` / `?auth_token=`. Either Claude Code variable works:
  - `ANTHROPIC_AUTH_TOKEN` sends `Authorization: Bearer` and shows **no** approval prompt **[probe]**.
  - `ANTHROPIC_API_KEY` sends `X-Api-Key` and shows a one-time interactive "use this key?" prompt, which defaults to **No**. You can pre-seed the answer.
- First-run prompts that can be skipped, all confirmed **[probe]**:
  - `hasCompletedOnboarding: true` in `~/.claude.json` skips the theme picker and onboarding.
  - `projects["<repo root>"].hasTrustDialogAccepted: true` skips the trust dialog.
  - `customApiKeyResponses.approved: ["<last 20 chars of key>"]` skips the API key prompt.
  - `skipDangerousModePermissionPrompt: true` in `~/.claude/settings.json` skips the bypass-mode warning.
  - Then launch with `claude --dangerously-skip-permissions`. Run as non-root, or set `IS_SANDBOX=1`.
- Egress: with a gateway, only the CLIProxyAPI host is strictly required. Set `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1`. `api.anthropic.com` is still contacted for the WebFetch domain check unless `skipWebFetchPreflight: true` is set.

## 1. Where each thing is discovered

`CLAUDE_CONFIG_DIR` relocates everything below, including `.claude.json` and `.credentials.json`. With it set, `.claude.json` lives at `$CLAUDE_CONFIG_DIR/.claude.json` rather than `~/.claude.json` (*claude-directory*, *authentication*, **[probe]**: the file was created inside the config dir). It is ignored in project or local `env` blocks (*settings-reference*, "Variables Claude Code ignores in env").

### User settings

| File | Scope |
|---|---|
| `~/.claude/settings.json` | user |
| `.claude/settings.json` | shared project |
| `.claude/settings.local.json` | project local. For a worktree it is read from the **main checkout's root**. |
| `managed-settings.json` | managed |
| `--settings <file-or-json>` | command line |

- Precedence, highest first: managed > `--settings` > local > project > user. Lists such as `permissions.allow` merge across files (*settings*).
- Installing Claude Code creates no settings file (*settings*).
- `permissions.defaultMode: "bypassPermissions"` and `"auto"` are ignored in project and local files. They only work from user or managed settings, or `--permission-mode` (*settings*, *permission-modes*).
- Hooks, `env`, `apiKeyHelper`, `statusLine` and similar keys can hold absolute host paths or secrets. This machine's own user `settings.json` has an `env` block.

### `CLAUDE.md`

- User level: `~/.claude/CLAUDE.md` and `~/.claude/rules/*.md`.
- Project level: `CLAUDE.md` and `CLAUDE.local.md` in the cwd and every ancestor directory, plus `.claude/rules/`. Subdirectory files load on demand.
- `@path` imports are followed. Imports that resolve outside the working directory trigger an approval dialog for project files, but not for user-scope files (*memory*).
- A `~/.claude/CLAUDE.md` that `@imports` files elsewhere under `$HOME` breaks when copied unless those files are copied too.

### Skills, agents and commands

| Kind | Locations |
|---|---|
| Skills | `~/.claude/skills/<name>/SKILL.md` (personal), `.claude/skills/` in the cwd and ancestors up to the repo or worktree root (project), managed dir, `--add-dir` dirs (*skills*) |
| Commands | `~/.claude/commands/*.md` and `.claude/commands/*.md`. Same mechanism as skills (*skills*). |
| Agents | `~/.claude/agents/*.md` and `.claude/agents/*.md` (*sub-agents*) |
| Output styles | `~/.claude/output-styles/*.md` |
| Workflows | `~/.claude/workflows/*.js` |
| Subagent memory | `agent-memory/` (*claude-directory*) |

- Skill entries may be **symlinks**. On this machine most `~/.claude/skills/*` entries point into `~/.codex/skills/`, so boxq must copy with symlink dereferencing (`cp -rL` or equivalent).
- `~/.claude/skills/synced/` is reserved for skills synced from claude.ai, which needs a claude.ai login.
- Plugins live in `~/.claude/plugins/`. `installed_plugins.json` there records **absolute `installPath`s** (this machine: `/opt/t3code/.claude/plugins/cache/...`), and plugins are enabled via `enabledPlugins` and `extraKnownMarketplaces` in settings.json. Copying plugins is out of the planned set. If `settings.json` is copied with `enabledPlugins` left in, Claude Code will try to fetch the marketplaces from GitHub inside the VM.

### MCP servers (*mcp*, "MCP installation scopes")

| Scope | Stored in | Notes |
|---|---|---|
| Local (default for `claude mcp add`) | `~/.claude.json` → `projects["<abs path>"].mcpServers` | Keyed by host path. boxq mounts the Workspace at the same path, so the key stays valid. |
| User | `~/.claude.json` → top-level `mcpServers` | Applies to all projects |
| Project | `.mcp.json` at the project root | Already inside the mounted Workspace, so nothing to copy |
| Plugin / claude.ai connectors | plugin `.mcp.json` / `mcp-proxy.anthropic.com` | Connectors need a claude.ai login, so they are not relevant here |

- Precedence: local > project > user > plugin > connectors. The whole entry wins and fields are not merged.
- Each project entry also carries `enabledMcpjsonServers` and `disabledMcpjsonServers` (*mcp*; key names confirmed on this machine).
- MCP entries are **not inherently credential-free**. `env` and `headers` can hold literal tokens, so prefer `${VAR}` expansion (*mcp*, "Environment variable expansion").
- MCP OAuth tokens are not stored in `.claude.json`. The binary has an `mcpOAuth` credential key **[binary]**, which suggests they live with the other credentials in `.credentials.json` or the keychain. Docs only say "stored securely".
- `-p` runs, and bypass-mode sessions with `skipDangerousModePermissionPrompt` set in user settings, connect `.mcp.json` servers without the approval prompt (*mcp*, project scope).

### Credentials

- Linux: `~/.claude/.credentials.json`, mode `0600`. On macOS they go to the Keychain. `/login` and `/logout` manage the file (*authentication*, "Credential management").
- `~/.claude.json` can also hold `primaryApiKey`, a Console-created key saved to config **[binary]**, and holds `oauthAccount` (account and org details; present on this machine).
- Also secret-bearing:
  - `settings.json` `env` and `apiKeyHelper`.
  - Transcripts under `~/.claude/projects/` (plaintext, may contain anything a tool printed).
  - `~/.config/anthropic` profiles.
  - Heap snapshots (*claude-directory*).

Authentication precedence (*authentication*, "Authentication precedence"):

1. Cloud provider (`CLAUDE_CODE_USE_BEDROCK` / `_VERTEX` / `_FOUNDRY`)
2. `ANTHROPIC_AUTH_TOKEN`
3. `ANTHROPIC_API_KEY`
4. `apiKeyHelper`
5. `CLAUDE_CODE_OAUTH_TOKEN`
6. Anthropic profiles
7. `/login` OAuth

### Machine-specific state under `~/.claude` (never copy)

`projects/` (transcripts and auto memory), `history.jsonl`, `file-history/`, `sessions/`, `session-env/`, `shell-snapshots/`, `tasks/`, `plans/`, `debug/`, `paste-cache/`, `backups/` (old copies of `.claude.json`, so they contain whatever it contains), `cache/`, `stats-cache.json`, `remote-settings.json`, `policy-limits.json`, `plugins/`, `jobs/`, `daemon/`, `usage-data/`, `feedback*` (*claude-directory*, "Application data").

In `~/.claude.json`, everything except `mcpServers` and `projects[*].mcpServers` is host state. On this machine the top-level keys are `installMethod`, `machineID`, `userID`, `oauthAccount`, `firstStartTime`, migration flags, many `*Cache` keys, `skillUsage`, `pluginUsage` and `projects`.

## 2. Recommended copy list and never-copy list

**Copy on every `boxq run`.** Write into the VM's `~/.claude/`, or into a boxq-managed `CLAUDE_CONFIG_DIR`.

| Host source | Note |
|---|---|
| `~/.claude/CLAUDE.md` | Resolve or warn on `@imports` outside `~/.claude` |
| `~/.claude/rules/` | User-level memory, same status as `CLAUDE.md` |
| `~/.claude/skills/`, `agents/`, `commands/`, `output-styles/` | Dereference symlinks. Skip `skills/synced/` and `.trash/`. |
| `~/.claude/settings.json`, **filtered** | Drop `env` (or allowlist keys), `apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport`, `otelHeadersHelper`, `statusLine` and `hooks` that reference host paths, `enabledPlugins` and `extraKnownMarketplaces` (unless plugins are deliberately supported). Then merge in boxq's keys (§4). |
| `~/.claude.json` → `mcpServers` | User scope |
| `~/.claude.json` → `projects[<workspace or repo root>].mcpServers`, `enabledMcpjsonServers`, `disabledMcpjsonServers` | Local scope for this Workspace only |

**Never copy:**
- `~/.claude/.credentials.json`.
- `~/.claude.json` as a whole. Build a fresh one instead (§4).
- `~/.claude/backups/`, `projects/`, `history.jsonl`, `file-history/`, `sessions/`, `session-env/`, `shell-snapshots/`, `plugins/`, `cache/`, `debug/`, `paste-cache/`, `stats-cache.json`, `remote-settings.json`, `policy-limits.json`, `jobs/`, `daemon/`.
- `~/.config/anthropic/`.

**Merge, don't overwrite.** Claude Code rewrites `.claude.json` itself, adding `numStartups`, `machineID`, `projects[...]` history and so on **[probe]**. boxq should merge its keys into the Sandbox's existing file on reattach rather than clobber it.

## 3. Base URL and API key (CLIProxyAPI)

- Both variables work with a custom `ANTHROPIC_BASE_URL` (*env-vars*, *llm-gateway-connect*):
  - `ANTHROPIC_AUTH_TOKEN` is sent as `Authorization: Bearer <value>`.
  - `ANTHROPIC_API_KEY` is sent as `X-Api-Key: <value>`.
  - `apiKeyHelper` output is sent in **both** headers.
  - "A credential in the wrong variable reaches the gateway in a header it doesn't read, and the request fails with `401`" (*llm-gateway-connect*).
- **CLIProxyAPI accepts all of them.** In `internal/access/config_access/provider.go`, `Authenticate()` reads `Authorization` (with `Bearer ` stripped), `X-Goog-Api-Key`, `X-Api-Key`, and the query params `key` / `auth_token`. It accepts the request if any candidate is in `access.api-keys` in `config.yaml`. The README does not document this; the source does.
- For BoxLite placeholder substitution, both headers carry the raw placeholder string. `Authorization` carries it after the `Bearer ` prefix.
- Interactive approval:
  - `ANTHROPIC_API_KEY` shows "Detected a custom API key in your environment … Do you want to use this API key?" with **No (recommended)** pre-selected **[probe]**.
  - The answer is stored in `~/.claude.json` `customApiKeyResponses.{approved,rejected}` as the **last 20 characters** of the key (`e.trim().slice(-20)`) **[binary]** **[probe]**.
  - Pre-seeding `approved` with the last 20 chars of the value *as seen inside the VM*, i.e. the placeholder, suppresses the prompt **[probe]**.
  - `ANTHROPIC_AUTH_TOKEN` never prompts **[probe]**.
  - In `-p` mode the API key is always used without prompting (*authentication*).
- Do not set both variables. Claude Code warns "Both ANTHROPIC_AUTH_TOKEN and ANTHROPIC_API_KEY set · auth may not work as expected" **[probe]**.
- With a custom base URL, Remote Control is disabled and the gateway credential takes precedence over any saved login (*env-vars*).
- **Recommendation:** keep `ANTHROPIC_API_KEY` as boxq's host-side config name, but export it into the VM as `ANTHROPIC_AUTH_TOKEN`. That avoids the approval prompt and the last-20-chars coupling to the placeholder format. If `ANTHROPIC_API_KEY` is kept, seed `customApiKeyResponses`.

## 4. Non-interactive start: bypass mode, no onboarding, theme or trust prompts

All verified with v2.1.312 in a fresh `CLAUDE_CONFIG_DIR` **[probe]**. Each prompt that appears without seeding, in order:
1. Theme / "Let's get started" onboarding.
2. Workspace trust dialog. It appears **even with** `--dangerously-skip-permissions`.
3. "Use this API key?" prompt (`ANTHROPIC_API_KEY` only).
4. Bypass Permissions warning.

With the seed below, the session opens straight to the prompt showing `⏵⏵ bypass permissions on`.

`~/.claude.json` (or `$CLAUDE_CONFIG_DIR/.claude.json`):
```json
{
  "hasCompletedOnboarding": true,
  "projects": { "<repo root>": { "hasTrustDialogAccepted": true } },
  "customApiKeyResponses": { "approved": ["<last 20 chars of in-VM key>"], "rejected": [] },
  "mcpServers": { "...": "user-scope servers copied from host" }
}
```

`~/.claude/settings.json` (merged into the filtered host copy):
```json
{
  "skipDangerousModePermissionPrompt": true,
  "permissions": { "defaultMode": "bypassPermissions" }
}
```

Launch: `claude --dangerously-skip-permissions`, or rely on `defaultMode`.

**Onboarding**
- Onboarding shows when `!hasCompletedOnboarding` **[binary]**. `hasCompletedOnboarding` and `lastOnboardingVersion` are not in the public settings reference.
- The theme picker is part of onboarding. The theme defaults to `dark`, and `theme` can also be set in settings.json (*settings-reference*).
- `IS_DEMO=1` also "skips onboarding" (*env-vars*) but hides account info, so it is not recommended.

**Trust**
- Set `projects["<path>"].hasTrustDialogAccepted: true` in `~/.claude.json` (*permissions*, "What runs before you trust a folder"; *mcp*).
- The key is the **git repository root**. For a **worktree** it is the **main checkout's root** (*permissions*, "Project allow rules and workspace trust"). boxq mounts the common `.git` at the same path, so it should seed the main checkout root's path, and the worktree path as well, to be safe.
- `-p` and SDK runs never show the dialog.

**Bypass**
- On first interactive use, accepting the warning writes `skipDangerousModePermissionPrompt: true` to `~/.claude/settings.json` (*permission-modes*). Pre-seeding it suppresses the warning **[probe]**.
- `bypassPermissions` from project or local settings is ignored.
- Managed `permissions.disableBypassPermissionsMode: "disable"` blocks bypass mode entirely. Don't copy managed settings.

**Root**
- Bypass is refused as root or under sudo: `--dangerously-skip-permissions cannot be used with root/sudo privileges`. The check is `getuid()===0 && IS_SANDBOX!=="1" && !CLAUDE_CODE_BUBBLEWRAP` **[binary]** (*permission-modes*: "skipped automatically inside a recognized sandbox").
- The Sandbox should run Claude Code as a non-root user, or export `IS_SANDBOX=1`.

**Non-interactive alternative**
- `claude -p … --dangerously-skip-permissions` shows no trust or bypass dialogs and uses `ANTHROPIC_API_KEY` without asking (*headless*, *permission-modes*, *authentication*). The onboarding screens are interactive UI.
- `--bare` additionally skips CLAUDE.md, skills, agents, MCP and plugins, so it is not wanted here.

## 5. Hosts Claude Code contacts, and auto-update

From *network-config*, "Network access requirements", plus *data-usage* and *llm-gateway-connect*:

| Host | Purpose | Needed by boxq? |
|---|---|---|
| CLIProxyAPI host (Tailscale HTTPS) | All model traffic via `ANTHROPIC_BASE_URL` | **Yes** |
| `api.anthropic.com` | API (not used with a gateway); WebFetch hostname safety preflight (any provider; disable with `skipWebFetchPreflight: true`); feature flags and telemetry events; fast-mode check (goes direct, never via the gateway) | Recommended. The map's built-in "Anthropic" entry covers it. Otherwise set `skipWebFetchPreflight`. |
| `registry.npmjs.org` | `npx` MCP servers, plugin deps, npm installs of Claude Code | Yes (already built in) |
| `github.com` | Plugin marketplaces, including **official marketplace auto-install** on first interactive launch (not covered by NONESSENTIAL) | Yes (already built in). Set `CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1`. |
| `raw.githubusercontent.com` | `/release-notes` changelog; background fetch in interactive mode after an update | No (NONESSENTIAL covers release notes) |
| `downloads.claude.ai` | Native installer, native auto-updater, version checks, plugin executables | Only at image build. Not at runtime. |
| `storage.googleapis.com` | Plugin install counts; auto-updater before v2.1.116 | No |
| `claude.ai`, `claude.com`, `platform.claude.com`, `sso.claude.com` | OAuth / Console login | No (no OAuth in Sandbox) |
| `mcp-proxy.anthropic.com` | claude.ai MCP connectors | No |
| `http-intake.logs.us5.datadoghq.com`, `browser-intake-us5-datadoghq.com` | Telemetry and error reports | No. Disable via `DISABLE_TELEMETRY` / NONESSENTIAL. |
| `code.claude.com` | Built-in docs lookups (claude-code-guide agent) | Optional |
| `*.frame.claudeusercontent.com`, `bridge.claudeusercontent.com` | Artifacts and Chrome bridge (claude.ai login only) | No |
| `formulae.brew.sh` | Homebrew update checks | No |
| MCP servers' own endpoints | Whatever each copied MCP server calls | Per-server; an open question on the map |

**Auto-update**
- Native installs auto-update in the background from `downloads.claude.ai`. npm installs update via `registry.npmjs.org`. apt, dnf, apk, Homebrew and WinGet do not auto-update (*setup*).
- Disable options:
  - `DISABLE_AUTOUPDATER=1` stops background updates only.
  - `DISABLE_UPDATES=1` also blocks `claude update` and `claude install`.
  - `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` also disables auto-updates, telemetry, error reporting, `/feedback`, release notes and feature-flag fetching. Any non-empty value counts, including `0` (*env-vars*).
- `autoUpdatesChannel` (`latest` / `stable`) and `minimumVersion` live in settings (*setup*).
- Bake a pinned version into the GHCR image and set `DISABLE_UPDATES=1`.

**Recommended Sandbox env:**
```
ANTHROPIC_BASE_URL=https://<cliproxy>.<tailnet>.ts.net
ANTHROPIC_AUTH_TOKEN=<BoxLite placeholder>   # or ANTHROPIC_API_KEY + seeded customApiKeyResponses
CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1
CLAUDE_CODE_DISABLE_OFFICIAL_MARKETPLACE_AUTOINSTALL=1
DISABLE_UPDATES=1
IS_SANDBOX=1                                  # only if Claude runs as root
```

**Recommended Egress Policy for Claude Code:**
- Required: the CLIProxyAPI host:443.
- Recommended: `api.anthropic.com` (WebFetch preflight).
- Already built in: `registry.npmjs.org`, `github.com`.
- Everything else in the table above can stay blocked once those env vars are set.
- If `api.anthropic.com` is dropped, set `skipWebFetchPreflight: true` or WebFetch fails.

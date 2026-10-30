# Yosh (`yoshell.ai`) Analysis & Litty Integration Plan (v0.7.0)

> **Target Release:** v0.7.0 (scheduled after v0.6.0 release)

## 1. Analysis of `yoshell.ai` (Yosh)

**Yosh** ([pizlonator/yosh](https://github.com/pizlonator/yosh)) is an LLM-enabled fork of GNU Bash 5.3 + GNU Readline 8.3 compiled with Fil-C. Its core philosophy is **safety-first, zero-friction natural language to shell**:

### Core Workflow & UX
1. **Trigger (`yo <natural language>`)**:
   - The user types `yo <intent or question>` directly at the standard shell prompt.
   - Readline intercepts `Enter` (`rl_yo_accept_line`) before the shell executes the line.
   - Built-in local commands (`yo reset`, `yo show last response`, `yo show documentation`) run immediately without calling the API.
2. **Thinking & Status Indicator**:
   - Displays `Thinking...` (or `Fetching model info...`, `Compacting...`, `[attempt N/10] Waiting...`) in italic cyan (`\x1b[3;36m`).
   - Pressing `Ctrl+C` at any point immediately cancels the in-flight HTTP request or backoff sleep and prints `Cancelled`.
3. **Four Normalized LLM Tool Actions**:
   - **`command` (`{ command, explanation, pending }`)**: Prints `explanation` in italic cyan, then **pre-fills** `command` directly into the live shell prompt buffer (`BUFFER` / `READLINE_LINE`). No confirmation modal, no auto-execution—the user presses `Enter` to run, edits it, or hits `Ctrl+C` to discard.
   - **`chat` (`{ response }`)**: Renders the Markdown explanation/answer inline in the terminal (cyan italic base, white non-italic code spans/blocks, bold highlights) and returns to a fresh prompt.
   - **`scrollback` (`{ lines }`)**: Agentic tool loop—the LLM asks for `N` lines of recent terminal scrollback (`~200` to `1000` lines), inspects the output/errors, and follows up with a `command` or `chat` response.
   - **`docs` (`{}`)** : Fetches built-in documentation about `yo` configuration and features so the LLM can answer meta-questions accurately.
4. **Multi-Step Continuation (`pending: true`)**:
   - For multi-step workflows (e.g., *"check if nginx is running and restart it if not"*), the LLM emits one command at a time with `pending: true`.
   - Once the user presses `Enter` and that command finishes executing, the next prompt hook automatically captures the latest scrollback and sends a `[continuation]` turn to the LLM to prefill the next step until `pending: false`.
   - Submitting an empty line, pressing `Ctrl+C`, or typing a new `yo ...` command cancels the continuation chain.
5. **Session Memory & Context Compaction**:
   - Remembers conversation exchanges per shell session (including whether suggested commands were actually executed or skipped).
   - When estimated tokens exceed 50% of the context window, compacts the oldest ~75% of turns via a tools-less summarization request (`Compacting...`) and keeps the newest ~25% verbatim.

---

## 2. Why Litty Terminal Can Do This *Better* Than Yosh

Yosh has to fork Bash/Readline and inject a **transparent PTY proxy + 1MB `mmap` ring buffer** just so Readline can read terminal output—and it only works in custom-compiled Bash on Linux.

**Litty Terminal is the terminal emulator itself**, which gives us massive architectural advantages:
- **Zero-copy Scrollback Awareness**: `Grid` (`src/grid/mod.rs`) already holds the entire scrollback ring buffer and tracks semantic prompt/output boundaries via **OSC 133 (`A`, `B`, `C`, `D`)** in `PromptMark` (`src/grid/prompt.rs`). No PTY proxy or duplicate memory buffer is needed!
- **Works Across Every Shell (zsh, bash, fish, nushell)**: Litty already injects zero-dotfile shell integration scripts via `src/shell_integration.rs`.
- **Zero External Crate Bloat**: Litty already has a battle-tested `curl` wrapper and zero-dependency JSON parser in `src/update.rs`, preserving the `< 15 MB RAM` and `0% idle CPU` invariants in `CLAUDE.md`.

---

## 3. Architecture Design for Litty

```mermaid
sequenceDiagram
    participant User
    participant Shell as Shell (zsh/bash/fish/nu)
    participant Grid as Litty Grid (OSC 778)
    participant AI as Litty AI Worker Thread
    participant LLM as LLM API (Anthropic / OpenAI / OpenRouter / Ollama)

    User->>Shell: Types `yo find files > 100MB` + Enter
    Shell->>Grid: Intercepts `yo ...`, emits `\e]778;query;<b64>\a`
    Shell->>Shell: Waits on FIFO / response or PTY control
    Grid->>AI: Wakes AppEvent, spawns background AI task (with CWD, OS, shell, last exit status)
    AI->>Grid: Writes `\r\e[K\e[3;36mThinking...\e[0m` to terminal
    AI->>LLM: POST request (tools: command, chat, scrollback, docs)
    opt LLM calls `scrollback` tool
        LLM-->>AI: tool_use: `scrollback(lines: 200)`
        AI->>Grid: Extracts sanitized scrollback lines from Grid
        AI->>LLM: Follow-up POST with tool_result
    end
    LLM-->>AI: `command("find . -size +100M", "Find files larger than 100MB", pending: false)`
    AI->>Grid: Prints cyan italic explanation + pre-fills shell line buffer
    User->>Shell: Reviews, edits, or presses Enter to run
```

### 3.1 Shell Integration & Command Prefill Mechanism
To make `yo <query>` feel 100% native like Readline—where `yo ...` does not pollute shell error exit codes and pre-fills the live line buffer (`BUFFER` in zsh, `READLINE_LINE` in bash, `commandline` in fish)—we combine **Shell Keybind/Widget Interception** with a custom **OSC 778 (`\e]778;...`)** protocol:

1. **Line-Accept Interception in `src/shell/`**:
   - **Zsh (`src/shell/litty-integration.zsh`)**: Wrap `accept-line` ZLE widget (`_litty_accept_line`). When `$BUFFER` matches `^yo([[:space:]]+.*)?$`:
     - Add `$BUFFER` to zsh history (`print -s -- "$BUFFER"`).
     - Print `$BUFFER` and newline so it remains visible in scrollback just like a normal command.
     - Clear `$BUFFER=""` and emit `\e]778;yo;<base64-query>\a` to Litty.
   - **Bash (`src/shell/litty.bash`)**: Define a `yo()` shell function AND bind a Readline macro/hook so `yo ...` invokes the OSC 778 bridge, plus uses `bind -x` / `READLINE_LINE` or TIOCSTI-free PTY input injection for prefill.
   - **Fish (`src/shell/litty-integration.fish`)**: Bind `Enter` (`\r` / `\n`) to a function that checks `string match -rq '^yo(\s+.*)?$' -- (commandline)`; if matched, emits OSC 778 and clears the commandline; otherwise runs `commandline -f execute`.
   - **Nushell (`src/shell/litty.nu`)**: Define a `def --env --wrapped yo [...rest]` command and `pre_prompt` hook.

2. **How Litty Renders Output & Prefills the Prompt**:
   - Because Litty controls both the **Grid** (display) and the **PTY master** (input to shell):
   - While the shell is waiting or after the shell redraws its prompt (`OSC 133;B`):
     - **Status Indicator (`Thinking...` / `Compacting...` / `[attempt N/10]`)**: Litty renders a lightweight inline status or feeds the ANSI status line directly before the prompt, and intercepts `Ctrl+C` in `handle_key` (`src/main.rs`) while an AI request is active on that pane to cancel the request immediately without sending `SIGINT` to kill the shell's line!
     - **Chat / Explanation Display**: Printed above the prompt using Yosh's exact Markdown-to-ANSI styling (`\x1b[3;36m` italic cyan base, `\x1b[0m` code spans, `\x1b[1m` bold).
     - **Command Prefill**: Once the shell is at `OSC 133;B` (prompt ready), Litty writes the pre-filled command bytes directly to `pane.pty.write(...)` (wrapped in bracketed paste if multi-line or sent as clean command text). Because it is written to the PTY master, Readline/ZLE/Fish receives it as if the user typed it—allowing full editing, `Ctrl+C` cancellation, or pressing `Enter` to execute!

3. **Execution Tracking & Multi-Step Continuation (`pending: true`)**:
   - When Litty pre-fills a command, it marks `pane_ai.prefilled_command = Some((cmd, is_pending))`.
   - When the user presses `Enter` in `handle_key` (`src/main.rs`), Litty compares `g.current_prompt_input()` with `prefilled_command`:
     - Records `executed = true` in the session exchange history.
     - If `is_pending == true`, sets `awaiting_continuation = true`.
   - When the command finishes and the shell emits `OSC 133;D;<exit_code>` followed by `OSC 133;B` (next prompt ready) in `src/grid/perform.rs`:
     - If `awaiting_continuation` is set, Litty automatically grabs the last 200 lines of scrollback (including the command output between `OSC 133;C` and `D`) and dispatches a `[continuation]` LLM turn!

---

## 4. Provider & Configuration Design

To satisfy both **Litty's Ghostty-style config** (`~/.litty/config`) and **Yosh's config** (`~/.yoconf` + `~/.<provider>key` files):

### 4.1 Supported Providers & API Styles (`src/ai/provider.rs`)
1. **Anthropic (`anthropic`)**:
   - Endpoint: `https://api.anthropic.com/v1/messages` (or custom `base_url`)
   - Default model: `claude-sonnet-4-5-20250929`
   - Features: Ephemeral prompt caching (`cache_control`), optional extended thinking (`budget_tokens`), server-side web search (`web_search_20250305`).
2. **OpenAI (`openai`)**:
   - Endpoint: `https://api.openai.com/v1/responses` (Responses API) or `/v1/chat/completions`
   - Default model: `gpt-5.2` / `gpt-4o`
   - Features: `prompt_cache_key: "litty"`, `reasoning.effort`, `web_search` tool.
3. **OpenAI-Compatible Chat Completions (`openrouter`, `deepseek`, `kimi`, `qwen`, `zai`, `ollama`)**:
   - **OpenRouter**: `https://openrouter.ai/api/v1/chat/completions` (with sticky routing & vendor-aware prompt caching).
   - **DeepSeek**: `https://api.deepseek.com/chat/completions` (default `deepseek-chat` / `deepseek-v4-flash`).
   - **Kimi**: `https://api.moonshot.ai/v1/chat/completions` (default `kimi-k2.5`).
   - **Qwen**: `https://dashscope-intl.aliyuncs.com/compatible-mode/v1/chat/completions` (default `qwen-plus`).
   - **Local Ollama / Custom**: `http://localhost:11434/v1/chat/completions` (zero API key required when `provider = ollama`).

### 4.2 Configuration Resolution Order
1. **`~/.litty/config` directives**:
   - `ai-provider = anthropic | openai | openrouter | deepseek | kimi | qwen | zai | ollama`
   - `ai-model = <model-name>`
   - `ai-api-key = <key>` (or env var `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `OPENROUTER_API_KEY`, etc.)
   - `ai-base-url = <url>`
   - `ai-thinking = off | minimal | low | medium | high | max`
   - `ai-web-search = true | false`
2. **Yosh `~/.yoconf` & Key Files Fallback**:
   - Automatically reads `~/.yoconf` and `~/.anthropickey`, `~/.openaikey`, `~/.openrouterkey`, `~/.deepseekkey`, `~/.kimikey`, `~/.qwenkey`, `~/.zaikey` so existing Yosh setups work out of the box.

---

## 5. Phased Implementation Plan

### Phase 1: Core `src/ai/` Module & JSON/HTTP Engine
- **Files to create/modify**:
  - `src/ai/mod.rs`: Session state per Pane (`AiSession`), conversation history, cancellation token (`Arc<AtomicBool>`), last HTTP response stash.
  - `src/ai/json.rs`: Extend the zero-dependency JSON parser from `src/update.rs` with a clean JSON serializer/builder and number/boolean/array accessors so we don't add `serde_json` or `reqwest` to `Cargo.toml`.
  - `src/ai/http.rs`: `curl`-based HTTP client supporting `POST` and `GET`, exponential backoff retry (10 attempts, 1s..60s cap on 408/429/5xx/transport errors), and instant cancellation when `cancel_flag` is set (kills child `curl` process immediately on `Ctrl+C`).
  - `src/ai/config.rs`: Parse `ai-*` keys in `src/config.rs` + fallback reader for `~/.yoconf` and `~/.<provider>key` files + environment variables (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, etc.).

### Phase 2: Provider Builders, Tool Schemas & Markdown ANSI Renderer
- **Files to create**:
  - `src/ai/prompt.rs`: System prompt composer (OS/distro detection, shell type `zsh`/`bash`/`fish`/`nu`, CWD, model & context info, tuned prompts for Claude/OpenAI/Kimi/OpenRouter, and built-in `yo` documentation).
  - `src/ai/provider.rs`:
    - Request builders and response parsers for **Anthropic Messages API**, **OpenAI Responses API**, and **OpenAI Chat Completions API** (OpenRouter, DeepSeek, Kimi, Qwen, Ollama).
    - Normalized internal tool call representation: `Command { command, explanation, pending }`, `Chat { response }`, `Scrollback { lines }`, `Docs`.
    - Prompt caching markers per provider.
  - `src/ai/markdown.rs`: Yosh-compatible terminal Markdown renderer converting `**bold**`, `*italic*`, `` `code` ``, `~~strike~~`, and fenced ` ``` ` blocks into ANSI SGR sequences (`\x1b[3;36m` base style).

### Phase 3: Grid Scrollback Extraction, OSC 778 Protocol & Event Loop Integration
- **Files to modify**:
  - `src/grid/mod.rs` & `src/grid/perform.rs`:
    - Add `Grid::extract_scrollback_lines(max_lines: usize) -> String`: extracts recent scrollback + visible screen lines above the current prompt, joining soft-wrapped lines cleanly.
    - Parse `OSC 778` (`\e]778;yo;<payload>\a`) in `osc_dispatch` and queue an `AiRequest` on the `Grid` for the main thread to pick up, plus track `OSC 133;D;<exit_code>` and `OSC 133;B` transitions for multi-step continuation.
  - `src/event.rs`:
    - Add `AppEvent::AiStatus { pane_id: u64, text: String }` and `AppEvent::AiResult { pane_id: u64, outcome: AiOutcome }`.
  - `src/tab.rs` & `src/main.rs`:
    - Attach `AiPaneState` to `Pane` (allocated lazily on first `yo` command so idle panes use 0 extra bytes).
    - Handle `Ctrl+C` during active `AiPaneState::Thinking` to set the cancel flag and print `Cancelled`.
    - Handle `AiResult`: render Markdown chat/explanation into the pane's `Grid` (or before prompt) and write pre-filled command bytes to `pane.pty`.

### Phase 4: Shell Integration (`yo <query>` in zsh, bash, fish, nushell) & Multi-Step Continuation
- **Files to modify**:
  - `src/shell/litty-integration.zsh`: ZLE `accept-line` wrapper + `yo` fallback function.
  - `src/shell/litty.bash`: Readline `bind -x` / `yo` function integration.
  - `src/shell/litty-integration.fish`: Keybind wrapper on `\r`/`\n` + `yo` function.
  - `src/shell/litty.nu`: `def --env --wrapped yo` command.
  - Implement multi-step continuation (`pending: true`) and context compaction (`Compacting...` at 50% context window threshold) + direct local commands (`yo reset`, `yo show last response`, `yo show documentation`).

### Phase 5: Verification & Tests
- Unit tests in `src/ai/` for:
  - Config & `~/.yoconf` parsing, provider auto-detection.
  - Anthropic, OpenAI Responses, and Chat Completions JSON request building & tool-call parsing.
  - Markdown-to-ANSI rendering (`**bold**`, `` `code` ``, `*italic*`).
  - `Grid::extract_scrollback_lines` and `OSC 778` dispatch in `src/grid/mod.rs`.
- Cross-platform compile & lint checks:
  - `cargo check` (macOS)
  - `cargo check --target x86_64-unknown-linux-gnu` (Linux parity)
  - `cargo test`
  - `cargo clippy --all-targets`

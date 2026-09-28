# ADR 008: Resolve the terminal PATH at Mac installation

Date: 2026-09-25. Status: implemented. Extends [003](003-execution-boundaries.md).

## Evidence and decision

The installed LaunchAgent supplied a fixed Homebrew/system PATH. A remote `node` command failed with exit 127 because the user's Node installation is selected by NVM in `.zshrc`. The OS account's actual login shell is `/bin/zsh`; a Bash login did not select Node. A clean interactive zsh login resolved Node 24.14.1, Cargo, Homebrew, user binaries and paths containing spaces. Its startup took approximately eight seconds in the local check.

Resolve the OS account's configured Bash/zsh interactive-login PATH once during `setup:macos --allow-shell`, before pairing or changing the installation. Save only the resulting PATH in the LaunchAgent. Preserve `/bin/sh -c` command semantics, cwd, all permission flags, existing pairing and `KeepAlive=false`. Re-running setup refreshes the snapshot after PATH or shell changes. No native-agent or Cloudflare Worker code change is needed.

## Alternatives and tradeoffs

Running an interactive login shell for every command repeatedly executes startup code, adds measured startup latency, changes command behavior and exposes other profile variables. Hardcoding the current NVM version breaks after version changes. Copying the complete installer/terminal environment risks propagating deployment credentials. Capturing only PATH at installation is the smallest change and uses the native agent's existing PATH inheritance; it intentionally needs another setup run after future PATH changes.

The probe uses a minimal HOME/USER/LOGNAME/SHELL/PATH/LANG environment rather than the installer's environment. Closed stdin prevents unattended prompts from consuming tool input. A random NUL-delimited record separates PATH from startup chatter. Output is bounded, stderr is discarded, and errors do not echo profile output. The twenty-second deadline and process-group cleanup cover normal descendants, not intentionally daemonized owner code. Startup files remain trusted executable owner configuration and can have their ordinary side effects. No PTY is created; profiles that require terminal interaction must be corrected by the owner. Unsupported shells or invalid/relative/empty PATH entries fail before replacing the installation, without silently reverting to the old incomplete PATH.

The resolver preserves PATH ordering, spaces and duplicate entries. It does not import aliases, shell functions or other exported variables. Read-only installations do not run shell startup at all. The existing command environment still retains only PATH, HOME and LANG.

## Validation

[Regression tests](../../tests/terminal-path.test.mjs) run real Bash and zsh startup against isolated homes, execute a synthetic PATH-only tool from noninteractive commands, and verify startup runs only once. They cover credential isolation, spaces/ordering, startup failure, missing results, invalid PATH, output overflow, deadline cleanup and invalid probe options. The full repository check and the installed agent's actual command lookup are separate release checks.

Shell startup semantics: [zsh startup files](https://zsh.sourceforge.io/Doc/Release/Files.html) and [GNU Bash startup files](https://www.gnu.org/software/bash/manual/html_node/Bash-Startup-Files.html).

Code: [PATH resolver](../../scripts/terminal-path.mjs), [Mac installer](../../scripts/install-agent.mjs).

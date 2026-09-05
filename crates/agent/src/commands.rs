pub struct SlashCommandSpec {
    pub usage: &'static str,
    pub description: &'static str,
    pub category: &'static str,
}

const BUILTIN_COMMANDS: &[SlashCommandSpec] = &[
    SlashCommandSpec {
        usage: "/tools",
        description: "List available tools",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/bash <command>",
        description: "Run a guarded local bash command directly",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/bash tasks",
        description: "List background bash tasks",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/bash read <task-id>",
        description: "Read a background bash task",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/bash stop <task-id>",
        description: "Stop a background bash task",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/python <code>",
        description: "Run guarded local Python directly",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/python --timeout <seconds> -- <code>",
        description: "Run local Python with an explicit timeout",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/notebook",
        description: "Open the in-app Python notebook (kernel shared with the agent)",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/notebook run --code <code> [--timeout <s>]",
        description: "Run one Python cell in the persistent notebook kernel",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/read <path>",
        description: "Read a project file directly",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/write <path> -- <content>",
        description: "Write a full file body directly",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/edit <path> --old -- <old> --new -- <new>",
        description: "Replace exact text inside a file",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/diff [path ...]",
        description: "Show git diff for the repo or selected paths",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/help",
        description: "Show available commands",
        category: "Reference",
    },
    SlashCommandSpec {
        usage: "/setup",
        description: "Run PRISM account setup inside the TUI",
        category: "Account",
    },
    SlashCommandSpec {
        usage: "/login",
        description: "Authenticate against the hosted platform",
        category: "Account",
    },
    SlashCommandSpec {
        usage: "/logout",
        description: "Clear stored platform account credentials",
        category: "Account",
    },
    SlashCommandSpec {
        usage: "/clear",
        description: "Clear conversation history",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/compact",
        description: "Compact older conversation context",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/sessions",
        description: "List saved sessions",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/session",
        description: "Show the current session",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/session resume [id|latest]",
        description: "Resume a saved session",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/session fork [name]",
        description: "Fork the current session",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/resume [id|latest]",
        description: "Alias for /session resume",
        category: "Session",
    },
    SlashCommandSpec {
        usage: "/context",
        description: "Show the live API-facing context summary",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/permissions",
        description: "Inspect tool access and blocking rules",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/permissions allow <tool>",
        description: "Auto-approve a tool for this session",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/permissions deny <tool>",
        description: "Block a tool for this session",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/permissions ask <tool>",
        description: "Clear a session override for a tool",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/memory",
        description: "Show recent session memory and pending work",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/files",
        description: "Show the files currently in focus",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/tasks",
        description: "Show the pending work inferred from the session",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/plan",
        description: "Enter or inspect plan mode",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/plan off",
        description: "Exit plan mode",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/plan accept",
        description: "Approve the current plan for execution",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/plan reject",
        description: "Reject the current plan and keep iterating",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/plan clear",
        description: "Clear the stored approved-plan context",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/model [id]",
        description: "Show or switch the LLM model",
        category: "Agent",
    },
    SlashCommandSpec {
        usage: "/status",
        description: "Open the runtime status screen",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/config",
        description: "Open configuration details",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/usage",
        description: "Open usage and budget details",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/doctor",
        description: "Show runtime diagnostics",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/models list",
        description: "List hosted LLM models for the active platform project",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/models search <query>",
        description: "Search hosted models by ID, name, or provider",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/models info <model-id>",
        description: "Inspect one hosted model from the active project catalog",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/models register <id> --provider <p> --input-price <usd/mtok> --output-price <usd/mtok> --context-window <tokens>",
        description: "Register a custom/self-hosted model (z.ai, vLLM, Ollama) in ~/.prism/models.toml; optional --base-url, --api-key-env, --max-output-tokens",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/deploy list",
        description: "List persistent deployments visible to the current auth context",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/deploy status <deployment-id>",
        description: "Inspect one deployment in a native screen",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/deploy health <deployment-id>",
        description: "Run a deployment health check and show the result",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/discourse list",
        description: "List platform discourse specs for the current account",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/discourse show <spec-id>",
        description: "Inspect one discourse spec",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/discourse run <spec-id> [--param key=value]",
        description: "Run one discourse workflow and inspect its event stream",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/discourse status <instance-id>",
        description: "Inspect a discourse instance",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/discourse turns <instance-id>",
        description: "Inspect stored discourse turns",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/billing",
        description: "Show credit balance, usage, and prices",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/use show",
        description: "Show the active chat target (hosted / local / provider)",
        category: "Settings",
    },
    SlashCommandSpec {
        usage: "/use list",
        description: "List every LLM provider PRISM can route chat to, and which have keys",
        category: "Settings",
    },
];

const CLI_BACKED_ROOTS: &[&str] = &[
    // `use` was MISSING while three shipped surfaces promised it:
    // `/help` registers `/use show` and `/use list` (and a test asserts it),
    // `prism use --help` says "Identical to the in-chat `/use` slash command",
    // and providers.toml tells the reader to run `/use provider ...` in the
    // TUI. Every one of them was answered with "Unsupported slash command
    // root: use". Measured live 2026-08-26 by typing it into the running TUI.
    "use",
    "setup",
    "login",
    "status",
    "workflow",
    "backend",
    "tools",
    "provision",
    "node",
    "ingest",
    "query",
    "agent",
    "run",
    "job-status",
    "mesh",
    "report",
    "marketplace",
    "research",
    "deploy",
    "models",
    "gpus",
    "discourse",
    "publish",
    "configure",
    // Capability roots surfaced in-app so `/<root> …` works WITHOUT leaving the
    // TUI (see docs/design/TUI_REACHABILITY_AUDIT.md + memory prism-no-exit-to-cli).
    // Each spawns `prism <root>` with a per-root timeout + graceful degradation;
    // long-lived subcommands (e.g. `campaign start`) get a proper pane/agent-tool
    // next. `use` is intentionally NOT here — it needs live chat-target hot-swap,
    // not a config-only subprocess that the running session would ignore.
    "compute",
    "campaign",
    // Durable wake-ups for long-running goals — reachable as `/schedule …`
    // inside the TUI, never "go run the CLI" (memory prism-no-exit-to-cli).
    "schedule",
    // The standard plugin contract's LIST surface, in-app: `/plugins list`
    // runs `prism plugins list` (same inventory as the agent's `plugins`
    // tool — one implementation, three doors).
    "plugins",
    "notebook",
    "pyiron",
    "billing",
    "federation",
    // The literature engine, in-app: the palette's papers forms dispatch
    // `/papers search|sweep|full-text|corpus …` (parity, 2026-09-05).
    "papers",
];

pub fn builtin_help_text() -> String {
    let mut lines = Vec::new();
    let categories = ["Reference", "Account", "Session", "Agent", "Settings"];

    for (index, category) in categories.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        lines.push(format!("{category}:"));
        for command in BUILTIN_COMMANDS
            .iter()
            .filter(|command| command.category == *category)
        {
            lines.push(format!("  {:<30} {}", command.usage, command.description));
        }
    }

    lines.push(String::new());
    lines.push("Most `prism` CLI subcommands also work here:".to_string());
    lines.push("  /query \"...\" [--json]".to_string());
    lines.push("  /workflow list".to_string());
    lines.push(
        "  /papers search --query \"...\" [--sources a,b] [--limit n]  (also sweep | full-text | corpus)"
            .to_string(),
    );
    lines.push("  /marketplace search <query>".to_string());
    lines.push("  /models list [--provider google]".to_string());
    lines.push("  /gpus".to_string());
    lines.push("  /discourse list".to_string());
    lines.push("  /campaign start --goal \"...\" [--budget 5]".to_string());
    lines.push("  /campaign status <id>".to_string());
    lines.push("  /schedule create --goal <id> --every 6h   (wake a goal back up)".to_string());
    lines.push("  /schedule list | cancel <id> | tick".to_string());
    lines.push("  /skills list".to_string());
    lines.push("  /plugins list   (every extension plane: loaded + failed)".to_string());
    lines.push("  /node up [--name x] | stop | status  (supervised in-app)".to_string());

    lines.join("\n")
}

pub fn is_cli_backed_slash_root(root: &str) -> bool {
    CLI_BACKED_ROOTS.contains(&root)
}

#[cfg(test)]
mod tests {
    use super::{builtin_help_text, is_cli_backed_slash_root};

    /// Every command `/help` advertises must actually be reachable.
    ///
    /// `/use show` and `/use list` were registered in the help table, asserted
    /// by `help_text_lists_core_commands`, documented by `prism use --help` as
    /// "identical to the in-chat /use slash command" — and rejected at runtime
    /// with "Unsupported slash command root: use", because the allowlist that
    /// gates dispatch is a SECOND list that nothing kept in sync with the
    /// first. Found by typing it into the running TUI, not by any test.
    #[test]
    fn the_papers_engine_is_a_dispatchable_slash_root() {
        // Parity, measured 2026-09-05: the palette's papers forms dispatch
        // `/papers …`, so the root must pass the allowlist gate.
        assert!(is_cli_backed_slash_root("papers"));
        assert!(
            builtin_help_text().contains("/papers search"),
            "help advertises it too"
        );
    }

    #[test]
    fn every_slash_root_that_help_advertises_is_dispatchable() {
        let help = builtin_help_text();
        let mut checked = 0usize;
        // Every root the help table advertises that ALSO names a `prism`
        // CLI subcommand must be dispatchable. Roots handled entirely inside
        // the agent (context, usage, help, …) never reach the allowlist, so
        // they are identified by not being CLI subcommands at all — the set
        // below is exactly the CLI's own command list.
        const CLI_SUBCOMMANDS: &[&str] = &[
            "use",
            "billing",
            "models",
            "marketplace",
            "mesh",
            "node",
            "deploy",
            "discourse",
            "workflow",
            "ingest",
            "query",
            "research",
            "compute",
            "status",
            "login",
            "setup",
            "tools",
            "report",
            "publish",
        ];
        for token in help.split_whitespace() {
            let Some(cmd) = token.strip_prefix('/') else {
                continue;
            };
            let root: String = cmd
                .chars()
                .take_while(|c| c.is_ascii_lowercase() || *c == '-')
                .collect();
            if !CLI_SUBCOMMANDS.contains(&root.as_str()) {
                continue;
            }
            checked += 1;
            assert!(
                is_cli_backed_slash_root(&root),
                "/help advertises `/{root}` and `prism {root}` exists, but the \
                 dispatch allowlist rejects it — a reader who follows the help \
                 gets \"Unsupported slash command root: {root}\""
            );
        }
        assert!(
            checked > 0,
            "no CLI-backed command was examined; the parse is broken and this \
             test would pass over anything"
        );
    }

    #[test]
    fn help_text_lists_core_commands() {
        let help = builtin_help_text();
        assert!(help.contains("/context"));
        assert!(help.contains("/usage"));
        assert!(help.contains("Most `prism` CLI subcommands also work here"));
        // Parity drain: billing visibility and the read-only /use show are
        // discoverable from /help, not just from the source.
        assert!(help.contains("/billing"));
        assert!(help.contains("/use show"));
        assert!(help.contains("/campaign start"));
        // A goal that runs for months needs its wake-ups reachable INSIDE
        // the TUI — never "go run the prism CLI".
        assert!(help.contains("/schedule create"));
        // Standard plugin contract: the LIST surface must be discoverable
        // from /help in-app, not just from the source.
        assert!(help.contains("/plugins list"));
    }

    #[test]
    fn cli_roots_match_expected_commands() {
        assert!(is_cli_backed_slash_root("workflow"));
        assert!(is_cli_backed_slash_root("status"));
        assert!(is_cli_backed_slash_root("gpus"));
        assert!(is_cli_backed_slash_root("schedule"));
        // The plugin inventory is a first-class in-app surface (standard
        // plugin contract, LIST rule).
        assert!(is_cli_backed_slash_root("plugins"));
        assert!(!is_cli_backed_slash_root("session"));
        assert!(!is_cli_backed_slash_root("permissions"));
    }
}

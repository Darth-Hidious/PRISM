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
        description: "Authenticate against the MARC27 platform",
        category: "Account",
    },
    SlashCommandSpec {
        usage: "/logout",
        description: "Clear stored MARC27 account credentials",
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
        description: "List hosted LLM models for the active MARC27 project",
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
];

const CLI_BACKED_ROOTS: &[&str] = &[
    "setup",
    "login",
    "status",
    "workflow",
    "backend",
    "tools",
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
    "discourse",
    "publish",
    "configure",
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
    lines.push("  /marketplace search <query>".to_string());
    lines.push("  /models list [--provider google]".to_string());
    lines.push("  /discourse list".to_string());

    lines.join("\n")
}

pub fn is_cli_backed_slash_root(root: &str) -> bool {
    CLI_BACKED_ROOTS.contains(&root)
}

#[cfg(test)]
mod tests {
    use super::{builtin_help_text, is_cli_backed_slash_root};

    #[test]
    fn help_text_lists_core_commands() {
        let help = builtin_help_text();
        assert!(help.contains("/context"));
        assert!(help.contains("/usage"));
        assert!(help.contains("Most `prism` CLI subcommands also work here"));
    }

    #[test]
    fn cli_roots_match_expected_commands() {
        assert!(is_cli_backed_slash_root("workflow"));
        assert!(is_cli_backed_slash_root("status"));
        assert!(!is_cli_backed_slash_root("session"));
        assert!(!is_cli_backed_slash_root("permissions"));
    }
}

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem};
use ratatui::Frame;

/// A command entry for the autocomplete palette
#[derive(Debug, Clone)]
pub struct CommandEntry {
    pub command: String,
    pub description: String,
    pub category: String,
}

/// Build the full command list (slash commands + workflows + tools)
pub fn all_commands() -> Vec<CommandEntry> {
    let mut cmds = Vec::new();

    // Slash commands
    let slash = [
        ("/tools", "Tool catalog", "System"),
        ("/models", "Browse hosted models", "Models"),
        ("/models list", "List all models", "Models"),
        ("/models search", "Search models", "Models"),
        ("/model", "Show/switch current model", "Models"),
        ("/status", "Session & config info", "System"),
        ("/context", "Prompt budget & usage", "System"),
        ("/help", "All available commands", "System"),
        ("/config", "View/edit prism.toml", "Settings"),
        ("/permissions", "Permission matrix", "Settings"),
        ("/usage", "Token usage & cost", "Settings"),
        ("/clear", "Clear chat history", "Chat"),
        ("/compact", "Compress conversation", "Chat"),
        ("/sessions", "List saved sessions", "Sessions"),
        ("/session resume", "Resume a session", "Sessions"),
        ("/deploy list", "List deployments", "Compute"),
        ("/deploy create", "Create deployment", "Compute"),
        ("/run", "Submit compute job", "Compute"),
        ("/job-status", "Check job status", "Compute"),
        ("/mesh discover", "Find LAN peers", "Mesh"),
        ("/mesh publish", "Share dataset", "Mesh"),
        ("/mesh subscribe", "Subscribe to peer", "Mesh"),
        ("/node status", "Node info", "Mesh"),
        ("/node up", "Start local node", "Mesh"),
        ("/node down", "Stop local node", "Mesh"),
        ("/workflow list", "List workflows", "Workflows"),
        ("/workflow run", "Execute workflow", "Workflows"),
        ("/marketplace search", "Browse marketplace", "Marketplace"),
        ("/marketplace install", "Install resource", "Marketplace"),
        ("/ingest", "Ingest data file", "Data"),
        ("/query", "Query knowledge graph", "Data"),
        ("/research", "Research loop", "Data"),
        ("/discourse list", "List discourses", "Research"),
        ("/discourse create", "Create discourse", "Research"),
        ("/publish", "Publish artifact", "Platform"),
        ("/report", "File bug report", "Platform"),
        ("/login", "Re-authenticate", "Auth"),
        ("/logout", "Sign out", "Auth"),
        ("/read", "Read a file", "Files"),
        ("/edit", "Edit a file", "Files"),
        ("/write", "Write a file", "Files"),
        ("/diff", "Git diff", "Files"),
        ("/bash", "Execute shell command", "Code"),
        ("/python", "Execute Python code", "Code"),
        // Provenance & history
        ("/scratchpad", "Agent action log", "System"),
        ("/transcript", "Full conversation", "System"),
        // BYOK / BYOC
        ("/config set", "Set config value", "Settings"),
        ("/billing", "Credit balance", "Billing"),
        ("/billing usage", "Usage breakdown", "Billing"),
        ("/billing topup", "Buy credits", "Billing"),
        // Compute backends
        ("/run --ssh", "BYOC via SSH", "Compute"),
        ("/run --k8s-context", "BYOC via Kubernetes", "Compute"),
        ("/run --slurm", "BYOC via SLURM", "Compute"),
        ("/deploy status", "Deployment status", "Compute"),
        ("/deploy health", "Deployment health", "Compute"),
        ("/deploy stop", "Stop deployment", "Compute"),
        // Discourse
        ("/discourse run", "Run discourse", "Research"),
        ("/discourse status", "Discourse status", "Research"),
        ("/discourse turns", "View turns", "Research"),
        // Mesh extras
        ("/mesh subscriptions", "Active subscriptions", "Mesh"),
        ("/mesh unsubscribe", "Unsubscribe", "Mesh"),
        ("/node probe", "Probe capabilities", "Mesh"),
        ("/node logs", "Stream node logs", "Mesh"),
        // Query modes
        ("/query --cypher", "Direct Cypher query", "Data"),
        ("/query --semantic", "Semantic search", "Data"),
        ("/query --platform", "Platform graph", "Data"),
        ("/query --federated", "Federated query", "Data"),
        ("/ingest --watch", "Watch directory", "Data"),
        ("/ingest --schema-only", "Schema detection", "Data"),
        // Tools & discovery
        ("/discover_capabilities", "Full capability scan", "System"),
        ("/models info", "Model details", "Models"),
    ];

    for (cmd, desc, cat) in slash {
        cmds.push(CommandEntry {
            command: cmd.to_string(),
            description: desc.to_string(),
            category: cat.to_string(),
        });
    }

    cmds
}

/// Filter commands by prefix (what the user typed after /)
pub fn filter_commands<'a>(commands: &'a [CommandEntry], query: &str) -> Vec<&'a CommandEntry> {
    if query.is_empty() {
        // Show all when just "/" is typed
        return commands.iter().collect();
    }
    let q = query.to_lowercase();
    commands
        .iter()
        .filter(|c| {
            c.command.to_lowercase().contains(&q)
                || c.description.to_lowercase().contains(&q)
                || c.category.to_lowercase().contains(&q)
        })
        .collect()
}

/// Draw the autocomplete popup above the input bar
pub fn draw(f: &mut Frame, filtered: &[&CommandEntry], selected_idx: usize, input_area: Rect) {
    if filtered.is_empty() {
        return;
    }

    // Show max 8 items
    let visible_count = filtered.len().min(8);
    let height = (visible_count as u16) + 2; // +2 for borders

    // Position above the input bar
    let popup_area = Rect {
        x: input_area.x,
        y: input_area.y.saturating_sub(height),
        width: input_area.width,
        height,
    };

    f.render_widget(Clear, popup_area);

    let items: Vec<ListItem> = filtered
        .iter()
        .take(8)
        .enumerate()
        .map(|(i, entry)| {
            let is_selected = i == selected_idx;
            let style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .bg(Color::Rgb(30, 50, 70))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Rgb(180, 180, 180))
            };

            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<24}", entry.command),
                    if is_selected {
                        Style::default()
                            .fg(Color::Cyan)
                            .bg(Color::Rgb(30, 50, 70))
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Cyan)
                    },
                ),
                Span::styled(format!(" {:<20}", entry.description), style),
                Span::styled(
                    format!(" [{}]", entry.category),
                    Style::default().fg(Color::Rgb(60, 60, 60)),
                ),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(50, 50, 50)))
        .title(Span::styled(
            format!(" Commands ({}) ", filtered.len()),
            Style::default().fg(Color::Rgb(100, 100, 100)),
        ));

    let list = List::new(items).block(block);
    f.render_widget(list, popup_area);
}

import React from "react";
import { Box, Text } from "ink";
import {
  PRIMARY, MUTED, SECONDARY, SUCCESS, WARNING, ERROR, ACCENT_CYAN, TEXT,
} from "../theme.js";

interface LLMStatus {
  connected: boolean;
  provider: string | null;
}

interface PluginStatus {
  count: number;
  available: boolean;
  names: string[];
}

interface CommandTool {
  name: string;
  registered: boolean;
  healthy: boolean | null;
}

interface CommandStatus {
  tools: CommandTool[];
  total: number;
  healthy_providers: number;
  total_providers: number;
}

interface SkillStatus {
  count: number;
  names: string[];
}

interface AccountStatus {
  signed_in: boolean;
  user_id: string | null;
  display_name: string | null;
  org_id: string | null;
  org_name: string | null;
  project_id: string | null;
  project_name: string | null;
  platform_url: string | null;
}

interface Status {
  llm: LLMStatus;
  plugins: PluginStatus;
  commands: CommandStatus;
  skills: SkillStatus;
  account: AccountStatus;
}

interface Props {
  version: string;
  status: Status;
  autoApprove: boolean;
}

function Dot({ ok }: { ok: boolean }) {
  return <Text color={ok ? SUCCESS : MUTED}>{ok ? "\u25cf" : "\u25cb"}</Text>;
}

/** Short tool name for display */
function shortName(name: string): string {
  const map: Record<string, string> = {
    search_materials: "search",
    query_materials_project: "MP",
    literature_search: "literature",
    predict_property: "predict",
    execute_python: "python",
    web_search: "web",
  };
  return map[name] || name;
}

function CrystalMascot() {
  return (
    <Box flexDirection="column" marginBottom={1}>
      <Text color={PRIMARY}>    ⬡ ⬡ ⬡</Text>
      <Text color={PRIMARY}>  ⬡ ⬢ ⬢ ⬢ ⬡</Text>
      <Text color={PRIMARY}>  ⬡ ⬢ ⬢ ⬢ ⬡</Text>
      <Text color={PRIMARY}>    ⬡ ⬡ ⬡</Text>
    </Box>
  );
}

export function Welcome({ version, status, autoApprove }: Props) {
  const { llm, plugins, commands, skills, account } = status;

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={MUTED}
      paddingX={1}
      marginBottom={1}
    >
      <CrystalMascot />
      <Text>
        <Text color={PRIMARY} bold>PRISM</Text>
        <Text color={MUTED}>{`  native shell  v${version}`}</Text>
      </Text>
      <Text color={TEXT}>Autonomous materials research and compute orchestration.</Text>
      <Text color={MUTED}>Ask for a task or start with /help, /status, /workflow, /sessions.</Text>
      <Text> </Text>

      <Text>
        <Dot ok={llm.connected} />{" "}
        <Text color={MUTED}>model</Text>{" "}
        {llm.connected ? (
          <Text color={TEXT}>
            <Text bold>{llm.provider}</Text> ready
          </Text>
        ) : (
          <Text color={ERROR}>not configured</Text>
        )}
      </Text>

      <Text>
        <Dot ok={commands.tools.some((t) => t.healthy)} />{" "}
        <Text color={MUTED}>tools</Text>{" "}
        {commands.tools.length > 0 ? (
          commands.tools.map((t, i) => (
            <React.Fragment key={t.name}>
              <Text color={t.registered ? (t.healthy ? SUCCESS : WARNING) : MUTED}>
                {shortName(t.name)}
              </Text>
              {i < commands.tools.length - 1 && <Text color={MUTED}> · </Text>}
            </React.Fragment>
          ))
        ) : (
          <Text color={MUTED}>none</Text>
        )}
      </Text>

      <Text>
        <Dot ok={skills.count > 0} /> <Text color={MUTED}>skills</Text>{" "}
        <Text color={TEXT}>{skills.count}</Text>
        <Text color={MUTED}>{`  plugins ${plugins.available ? plugins.count : 0}`}</Text>
      </Text>

      <Text>
        <Dot ok={account.signed_in} /> <Text color={MUTED}>account</Text>{" "}
        {account.signed_in ? (
          <Text color={TEXT}>
            <Text bold>{account.display_name || account.user_id || "signed in"}</Text>
            {account.org_name || account.org_id ? (
              <Text color={MUTED}>{`  ·  ${account.org_name || account.org_id}`}</Text>
            ) : null}
            {account.project_name || account.project_id ? (
              <Text color={MUTED}>{`  ·  ${account.project_name || account.project_id}`}</Text>
            ) : null}
          </Text>
        ) : (
          <Text color={ERROR}>not signed in</Text>
        )}
      </Text>

      <Text color={MUTED}>
        {commands.healthy_providers}/{commands.total_providers} providers healthy
        {autoApprove ? "  ·  auto-approve ON" : ""}
      </Text>

      {!llm.connected ? (
        <Text color={MUTED}>
          Configure keys with <Text color={ACCENT_CYAN}>prism setup</Text> or via{" "}
          <Text color={SECONDARY}>platform.marc27.com</Text>.
        </Text>
      ) : null}
    </Box>
  );
}

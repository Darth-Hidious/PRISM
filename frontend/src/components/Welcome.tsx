import React from "react";
import { Box, Text } from "ink";
import {
  PRIMARY, MUTED, SECONDARY, SUCCESS, WARNING, ERROR, DIM, ACCENT_CYAN,
  CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
  RAINBOW, HEADER_COMMANDS_L, HEADER_COMMANDS_R,
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

interface Status {
  llm: LLMStatus;
  plugins: PluginStatus;
  commands: CommandStatus;
  skills: SkillStatus;
}

interface Props {
  version: string;
  status: Status;
  autoApprove: boolean;
}

function Dot({ ok }: { ok: boolean }) {
  return <Text color={ok ? SUCCESS : MUTED}>{ok ? "\u25cf" : "\u25cb"}</Text>;
}

/** Crystal mascot row — outer dim hexagons */
function CrystalOuterRow() {
  return (
    <Text>
      {"    "}
      <Text color={CRYSTAL_OUTER_DIM}>{"\u2b21"}</Text>
      {" "}
      <Text color={CRYSTAL_OUTER_DIM}>{"\u2b21"}</Text>
      {" "}
      <Text color={CRYSTAL_OUTER_DIM}>{"\u2b21"}</Text>
    </Text>
  );
}

/** Crystal mascot middle row + rainbow rays + commands */
function CrystalMiddleRow({ commands }: { commands: string[] }) {
  return (
    <Text>
      {"  "}
      <Text color={CRYSTAL_OUTER}>{"\u2b21"}</Text>
      {" "}
      <Text color={CRYSTAL_INNER} bold>{"\u2b22"}</Text>
      {" "}
      <Text color={CRYSTAL_CORE} bold>{"\u2b22"}</Text>
      {" "}
      <Text color={CRYSTAL_INNER} bold>{"\u2b22"}</Text>
      {" "}
      <Text color={CRYSTAL_OUTER}>{"\u2b21"}</Text>
      {"  "}
      {RAINBOW.map((c, i) => (
        <Text key={i} color={c} bold>{"\u2501"}</Text>
      ))}
      {"  "}
      {commands.map((cmd, i) => (
        <Text key={i} color={MUTED}>{cmd} </Text>
      ))}
    </Text>
  );
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

export function Welcome({ version, status, autoApprove }: Props) {
  const { llm, plugins, commands, skills } = status;

  return (
    <Box flexDirection="column" marginY={1}>
      {/* Crystal mascot */}
      <CrystalOuterRow />
      <CrystalMiddleRow commands={HEADER_COMMANDS_L} />
      <CrystalMiddleRow commands={HEADER_COMMANDS_R} />
      <CrystalOuterRow />

      <Text> </Text>

      {/* Title + version */}
      <Text>
        {"  "}
        <Text color={PRIMARY} bold>PRISM</Text>
        <Text dimColor>{` v${version}`}</Text>
      </Text>
      <Text dimColor>{"  AI-Native Autonomous Materials Discovery"}</Text>

      <Text> </Text>

      {/* 1. LLM */}
      <Text>
        {"  "}
        <Dot ok={llm.connected} />
        {" "}
        {llm.connected ? (
          <Text>
            <Text bold>{llm.provider}</Text>
            <Text dimColor> connected</Text>
          </Text>
        ) : (
          <Text>
            <Text color={ERROR}>No LLM configured</Text>
            <Text dimColor> — run </Text>
            <Text color={ACCENT_CYAN}>prism setup</Text>
            <Text dimColor> or visit </Text>
            <Text color={SECONDARY}>platform.marc27.com</Text>
          </Text>
        )}
      </Text>

      {/* 2. Plugins */}
      <Text>
        {"  "}
        <Dot ok={false} />
        {" "}
        <Text dimColor>Plugins</Text>
        <Text color={MUTED}> coming soon</Text>
      </Text>

      {/* 3. Commands */}
      <Text>
        {"  "}
        <Dot ok={commands.tools.some((t) => t.healthy)} />
        {" "}
        <Text dimColor>Commands </Text>
        {commands.tools.map((t, i) => (
          <React.Fragment key={t.name}>
            <Text color={t.registered ? (t.healthy ? SUCCESS : WARNING) : MUTED}>
              {shortName(t.name)}
            </Text>
            {i < commands.tools.length - 1 && <Text dimColor> · </Text>}
          </React.Fragment>
        ))}
        {commands.healthy_providers > 0 && (
          <Text dimColor>
            {" "}({commands.healthy_providers}/{commands.total_providers} providers)
          </Text>
        )}
      </Text>

      {/* 4. Skills */}
      <Text>
        {"  "}
        <Dot ok={skills.count > 0} />
        {" "}
        <Text dimColor>{skills.count} skills</Text>
      </Text>

      {autoApprove && (
        <Text>
          {"  "}
          <Text color={WARNING}>{"\u26a0"} auto-approve enabled</Text>
        </Text>
      )}
    </Box>
  );
}

import React from "react";
import { Box, Text } from "ink";
import {
  PRIMARY, TEXT, TEXT_MUTED, TEXT_DIM, SUCCESS, ERROR, WARNING,
  SECONDARY, BORDER,
} from "../theme.js";

interface LLMStatus {
  connected: boolean;
  provider: string | null;
}

interface CommandStatus {
  tools: { name: string; registered: boolean; healthy: boolean | null }[];
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
  display_name: string | null;
  org_name: string | null;
  org_id: string | null;
}

interface Status {
  llm: LLMStatus;
  plugins: { count: number; available: boolean };
  commands: CommandStatus;
  skills: SkillStatus;
  account: AccountStatus;
}

interface Props {
  version: string;
  status: Status;
  autoApprove: boolean;
}

function Check({ ok, label, detail }: { ok: boolean; label: string; detail?: string }) {
  return (
    <Text>
      <Text color={ok ? SUCCESS : TEXT_DIM}>{ok ? "●" : "○"}</Text>
      <Text color={TEXT_MUTED}>{" "}{label}</Text>
      {detail ? <Text color={TEXT_DIM}>{" "}{detail}</Text> : null}
    </Text>
  );
}

export function Welcome({ version, status, autoApprove }: Props) {
  const { llm, commands, skills, account } = status;
  const toolCount = commands.tools.filter((t) => t.registered).length;

  return (
    <Box flexDirection="column" paddingLeft={1} marginBottom={1}>
      {/* Title — one line, no animation */}
      <Box>
        <Text bold color={PRIMARY}>PRISM</Text>
        <Text color={TEXT_DIM}> v{version}</Text>
        <Text color={TEXT_DIM}> · </Text>
        <Text color={TEXT_MUTED}>materials research platform</Text>
      </Box>

      {/* Status checks — compact row */}
      <Box gap={2} marginTop={0}>
        <Check ok={llm.connected} label="model" detail={llm.provider ?? undefined} />
        <Check ok={toolCount > 0} label={`${toolCount} tools`} />
        <Check ok={skills.count > 0} label={`${skills.count} skills`} />
        <Check
          ok={account.signed_in}
          label="account"
          detail={account.signed_in ? account.display_name ?? undefined : undefined}
        />
      </Box>

      {/* Contextual hints — only when something needs attention */}
      {!llm.connected ? (
        <Box marginTop={0}>
          <Text color={TEXT_MUTED}>
            {"  run "}
            <Text color={SECONDARY}>prism login</Text>
            {" to connect"}
          </Text>
        </Box>
      ) : null}

      {autoApprove ? (
        <Text color={WARNING}>  △ auto-approve enabled</Text>
      ) : null}
    </Box>
  );
}

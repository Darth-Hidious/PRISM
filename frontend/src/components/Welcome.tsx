import React from "react";
import { Box, Text } from "ink";
import {
  PRIMARY, TEXT_MUTED, TEXT_DIM, SUCCESS, WARNING, SECONDARY,
} from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { Divider } from "./chrome/Divider.js";

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
  status?: Status;
  autoApprove?: boolean;
  toolCount?: number;
  sessionId?: string;
  resumed?: boolean;
  resumedMessages?: number;
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

export function Welcome({
  version,
  status,
  autoApprove,
  toolCount,
  sessionId,
  resumed,
  resumedMessages,
}: Props) {
  const llm = status?.llm;
  const commands = status?.commands;
  const skills = status?.skills;
  const account = status?.account;
  const registeredToolCount =
    commands?.tools?.filter((t) => t.registered).length ?? toolCount ?? 0;
  const visibleSkillCount = skills?.count ?? 0;

  return (
    <Box flexDirection="column" paddingLeft={1} marginBottom={1}>
      <Text bold color={PRIMARY}>PRISM</Text>
      <Text color={TEXT_DIM}>
        <Byline>
          <Text>{`v${version}`}</Text>
          <Text color={TEXT_MUTED}>materials research platform</Text>
        </Byline>
      </Text>
      <Divider color={TEXT_DIM} padding={2} />

      <Box gap={2} marginTop={0}>
        {llm ? (
          <Check ok={llm.connected} label="model" detail={llm.provider ?? undefined} />
        ) : null}
        <Check ok={registeredToolCount > 0} label={`${registeredToolCount} tools`} />
        <Check ok={visibleSkillCount > 0} label={`${visibleSkillCount} skills`} />
        <Check
          ok={!!account?.signed_in}
          label="account"
          detail={account?.signed_in ? account.display_name ?? undefined : undefined}
        />
      </Box>

      {sessionId ? (
        <Box marginTop={0}>
          <Text color={TEXT_DIM}>
            {resumed ? "  resumed session " : "  session "}
            <Text color={TEXT_MUTED}>{sessionId.slice(0, 12)}</Text>
            {resumed && resumedMessages !== undefined ? (
              <Text color={TEXT_DIM}>{` · ${resumedMessages} restored messages`}</Text>
            ) : null}
          </Text>
        </Box>
      ) : null}

      {llm && !llm.connected ? (
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

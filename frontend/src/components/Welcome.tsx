import React from "react";
import { Box, Text } from "ink";
import { PRIMARY, MUTED, SECONDARY, SUCCESS } from "../theme.js";

interface Props {
  version: string;
  provider: string;
  capabilities: Record<string, boolean>;
  toolCount: number;
  skillCount: number;
  autoApprove: boolean;
}

export function Welcome({ version, provider, capabilities, toolCount, skillCount, autoApprove }: Props) {
  return (
    <Box flexDirection="column" marginY={1}>
      <Text color={PRIMARY} bold>{"  PRISM"}</Text>
      <Text dimColor>{`  v${version}`}</Text>
      <Text dimColor>{"  AI-Native Autonomous Materials Discovery"}</Text>
      <Box marginTop={1}>
        <Text>{"  "}</Text>
        {provider && <Text color={SECONDARY} bold>{provider}</Text>}
        {provider && <Text dimColor>{" \u2502 "}</Text>}
        {Object.entries(capabilities).map(([name, ok]) => (
          <React.Fragment key={name}>
            <Text dimColor>{name} </Text>
            <Text color={ok ? SUCCESS : MUTED}>{ok ? "\u25cf" : "\u25cb"}</Text>
            <Text>{"  "}</Text>
          </React.Fragment>
        ))}
        <Text dimColor>{`${toolCount} tools`}</Text>
        {skillCount > 0 && <Text dimColor>{` \u2502 ${skillCount} skills`}</Text>}
      </Box>
    </Box>
  );
}

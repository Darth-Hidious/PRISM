import React from "react";
import { Box, Text } from "ink";
import {
  PRIMARY, MUTED, SECONDARY, SUCCESS, WARNING, DIM,
  CRYSTAL_OUTER_DIM, CRYSTAL_OUTER, CRYSTAL_INNER, CRYSTAL_CORE,
  RAINBOW, HEADER_COMMANDS_L, HEADER_COMMANDS_R,
} from "../theme.js";

interface Props {
  version: string;
  provider: string;
  capabilities: Record<string, boolean>;
  toolCount: number;
  skillCount: number;
  autoApprove: boolean;
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

export function Welcome({ version, provider, capabilities, toolCount, skillCount, autoApprove }: Props) {
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

      {/* Subtitle */}
      <Text dimColor>{"  AI-Native Autonomous Materials Discovery"}</Text>

      <Text> </Text>

      {/* Capabilities bar */}
      <Text>
        {"  "}
        {provider && <Text color={SECONDARY} bold>{provider}</Text>}
        {provider && <Text dimColor>{" \u00b7 "}</Text>}
        {Object.entries(capabilities).map(([name, ok]) => (
          <React.Fragment key={name}>
            <Text dimColor>{name} </Text>
            <Text color={ok ? SUCCESS : MUTED}>{ok ? "\u25cf" : "\u25cb"}</Text>
            <Text>{"  "}</Text>
          </React.Fragment>
        ))}
        <Text dimColor>{`${toolCount} tools`}</Text>
        {skillCount > 0 && <Text dimColor>{` \u00b7 ${skillCount} skills`}</Text>}
        {autoApprove && <Text dimColor>{" \u00b7 "}</Text>}
        {autoApprove && <Text color={WARNING}>{"auto-approve"}</Text>}
      </Text>
    </Box>
  );
}

import React from "react";
import { Box, Text } from "ink";
import { Divider } from "./Divider.js";
import { TEXT, TEXT_DIM } from "../../theme.js";

interface Props {
  children: React.ReactNode;
  color?: string;
  title?: React.ReactNode;
  subtitle?: React.ReactNode;
  footer?: React.ReactNode;
  paddingX?: number;
}

export function Pane({
  children,
  color,
  title,
  subtitle,
  footer,
  paddingX = 2,
}: Props) {
  return (
    <Box flexDirection="column" marginTop={1}>
      <Divider color={color} />
      <Box flexDirection="column" paddingX={paddingX}>
        {title || subtitle ? (
          <Box flexDirection="column" marginTop={1}>
            {title ? (
              <Text color={TEXT} bold>
                {title}
              </Text>
            ) : null}
            {subtitle ? <Text color={TEXT_DIM}>{subtitle}</Text> : null}
          </Box>
        ) : null}
        <Box flexDirection="column" marginTop={title || subtitle ? 1 : 0}>
          {children}
        </Box>
        {footer ? (
          <Box marginTop={1} flexDirection="column">
            {typeof footer === "string" ? <Text color={TEXT_DIM}>{footer}</Text> : footer}
          </Box>
        ) : null}
      </Box>
    </Box>
  );
}

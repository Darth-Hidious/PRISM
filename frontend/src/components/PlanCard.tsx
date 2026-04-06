import React from "react";
import { Box, Text } from "ink";
import { ACCENT, TEXT_MUTED } from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";

interface Props {
  content: string;
}

export function PlanCard({ content }: Props) {
  return (
    <Pane
      color={ACCENT}
      title="Plan"
      footer={
        <Text color={TEXT_MUTED}>
          <Byline>
            <KeyboardShortcutHint shortcut="y" action="execute" />
            <KeyboardShortcutHint shortcut="n" action="reject" />
          </Byline>
        </Text>
      }
    >
      <Box>
        <MarkdownText text={content} />
      </Box>
    </Pane>
  );
}

import React from "react";
import { Box, Text, useInput } from "ink";
import {
  ACCENT,
  PRIMARY,
  TEXT,
  TEXT_DIM,
  WARNING,
} from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";
import type { UiViewTab } from "../bridge/types.js";

interface Props {
  title: string;
  body?: string;
  tone?: string;
  tabs?: UiViewTab[];
  selectedTab?: string;
  footer?: string;
  onClose: () => void;
}

function borderColorForTone(tone?: string): string {
  switch (tone) {
    case "warning":
      return WARNING;
    case "accent":
      return ACCENT;
    default:
      return PRIMARY;
  }
}

function findInitialTabIndex(tabs: UiViewTab[], selectedTab?: string): number {
  if (!tabs.length || !selectedTab) {
    return 0;
  }

  const index = tabs.findIndex((tab) => tab.id === selectedTab);
  return index >= 0 ? index : 0;
}

export function CommandView({
  title,
  body,
  tone,
  tabs,
  selectedTab,
  footer,
  onClose,
}: Props) {
  const tabList = tabs ?? [];
  const [tabIndex, setTabIndex] = React.useState(() =>
    findInitialTabIndex(tabList, selectedTab),
  );

  React.useEffect(() => {
    setTabIndex(findInitialTabIndex(tabList, selectedTab));
  }, [tabList, selectedTab]);

  const activeTab = tabList[tabIndex];
  const activeBody = activeTab?.body ?? body ?? "";
  const activeTone = activeTab?.tone ?? tone;

  useInput((input, key) => {
    if (key.escape || key.return || input === "q" || input === "Q") {
      onClose();
      return;
    }

    if (tabList.length <= 1) {
      return;
    }

    if (key.leftArrow || (key.shift && key.tab)) {
      setTabIndex((index) => (index - 1 + tabList.length) % tabList.length);
      return;
    }

    if (key.rightArrow || key.tab) {
      setTabIndex((index) => (index + 1) % tabList.length);
      return;
    }

    const numeric = Number.parseInt(input, 10);
    if (!Number.isNaN(numeric) && numeric >= 1 && numeric <= tabList.length) {
      setTabIndex(numeric - 1);
    }
  });

  return (
    <Box
      flexDirection="column"
      borderStyle="round"
      borderColor={borderColorForTone(activeTone)}
      paddingX={1}
      paddingY={0}
      marginTop={1}
    >
      <Box justifyContent="space-between">
        <Text color={TEXT} bold>
          {title}
        </Text>
        <Text color={TEXT_DIM}>
          {tabList.length > 1 ? "tab switch" : "esc close"}
        </Text>
      </Box>
      {tabList.length > 0 ? (
        <Box marginTop={1} gap={1} flexWrap="wrap">
          {tabList.map((tab, index) => {
            const isActive = index === tabIndex;
            return (
              <Text
                key={tab.id}
                color={isActive ? TEXT : TEXT_DIM}
                inverse={isActive}
              >
                {index + 1}. {tab.title}
              </Text>
            );
          })}
        </Box>
      ) : null}
      <Box marginTop={1} flexDirection="column">
        <MarkdownText text={activeBody} />
      </Box>
      {footer ? (
        <Box marginTop={1}>
          <Text color={TEXT_DIM}>{footer}</Text>
        </Box>
      ) : null}
    </Box>
  );
}

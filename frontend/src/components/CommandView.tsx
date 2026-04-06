import React from "react";
import { Box, Text, useInput } from "ink";
import {
  PRIMARY,
  TEXT,
  TEXT_DIM,
  WARNING,
} from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";
import type { UiViewTab } from "../bridge/types.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";
import { Pill } from "./chrome/Pill.js";

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
  const footerGuide = (
    <Text color={TEXT_DIM}>
      <Byline>
        {tabList.length > 1 ? (
          <KeyboardShortcutHint shortcut="tab/shift+tab" action="switch tabs" />
        ) : null}
        <KeyboardShortcutHint shortcut="1-9" action="jump" />
        <KeyboardShortcutHint shortcut="esc" action="close" />
      </Byline>
    </Text>
  );

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
    <Pane
      color={borderColorForTone(activeTone)}
      title={title}
      subtitle={activeTab ? activeTab.title : undefined}
      footer={
        <Box flexDirection="column">
          {footer ? <Text color={TEXT_DIM}>{footer}</Text> : null}
          {footerGuide}
        </Box>
      }
    >
      {tabList.length > 0 ? (
        <Box gap={1} flexWrap="wrap">
          {tabList.map((tab, index) => {
            const isActive = index === tabIndex;
            return (
              <Pill
                key={tab.id}
                label={`${index + 1}. ${tab.title}`}
                color={isActive ? TEXT : TEXT_DIM}
                active={isActive}
              />
            );
          })}
        </Box>
      ) : null}
      <Box marginTop={tabList.length > 0 ? 1 : 0} flexDirection="column">
        <MarkdownText text={activeBody} />
      </Box>
    </Pane>
  );
}

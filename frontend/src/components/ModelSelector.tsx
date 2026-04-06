import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import { PRIMARY, MUTED, TEXT, DIM, SUCCESS } from "../theme.js";
import { Byline } from "./chrome/Byline.js";
import { KeyboardShortcutHint } from "./chrome/KeyboardShortcutHint.js";
import { Pane } from "./chrome/Pane.js";

interface ModelEntry {
  id: string;
  provider: string;
  context_window: number;
  input_price: number;
  output_price: number;
  supports_tools: boolean;
  supports_thinking: boolean;
  local: boolean;
  size_gb?: number;
}

interface Props {
  current: string;
  models: ModelEntry[];
  onSelect: (modelId: string) => void;
  onCancel: () => void;
}

function formatPrice(price: number): string {
  if (price === 0) return "free";
  return `$${price.toFixed(2)}/M`;
}

function formatCtx(ctx: number): string {
  if (ctx === 0) return "";
  if (ctx >= 1_000_000) return `${(ctx / 1_000_000).toFixed(0)}M`;
  return `${(ctx / 1_000).toFixed(0)}k`;
}

export function ModelSelector({ current, models, onSelect, onCancel }: Props) {
  const [selected, setSelected] = useState(() => {
    const idx = models.findIndex((m) => m.id === current);
    return idx >= 0 ? idx : 0;
  });

  // Group models by provider
  const providers = [...new Set(models.map((m) => m.provider))];

  useInput((input, key) => {
    if (key.upArrow) {
      setSelected((s) => (s - 1 + models.length) % models.length);
    } else if (key.downArrow) {
      setSelected((s) => (s + 1) % models.length);
    } else if (key.return) {
      onSelect(models[selected]!.id);
    } else if (key.escape || input === "q") {
      onCancel();
    }
  });

  return (
    <Pane
      color={PRIMARY}
      title="Model"
      subtitle={`current: ${current}`}
      footer={
        <Text color={DIM}>
          <Byline>
            <KeyboardShortcutHint shortcut="↑/↓" action="navigate" />
            <KeyboardShortcutHint shortcut="enter" action="select" />
            <KeyboardShortcutHint shortcut="esc" action="cancel" />
          </Byline>
        </Text>
      }
    >
      {providers.map((provider) => {
        const providerModels = models.filter((m) => m.provider === provider);
        return (
          <Box key={provider} flexDirection="column" marginTop={1}>
            <Text color={MUTED} bold>{provider.toUpperCase()}</Text>
            {providerModels.map((m) => {
              const globalIdx = models.indexOf(m);
              const isSelected = globalIdx === selected;
              const isCurrent = m.id === current;
              const ctx = formatCtx(m.context_window);
              const priceIn = formatPrice(m.input_price);
              const priceOut = formatPrice(m.output_price);
              const badges: string[] = [];
              if (m.supports_thinking) badges.push("think");
              if (m.supports_tools) badges.push("tools");
              if (m.local) badges.push(`${m.size_gb ?? "?"}GB`);

              return (
                <Box key={m.id}>
                  <Text color={isSelected ? PRIMARY : DIM}>
                    {isSelected ? " ❯ " : "   "}
                  </Text>
                  <Text
                    color={isCurrent ? SUCCESS : isSelected ? TEXT : MUTED}
                    bold={isSelected}
                  >
                    {m.id}
                  </Text>
                  {ctx ? <Text color={DIM}>{`  ${ctx}`}</Text> : null}
                  {!m.local ? (
                    <Text color={DIM}>{`  ${priceIn}→${priceOut}`}</Text>
                  ) : null}
                  {badges.length > 0 ? (
                    <Text color={DIM}>{`  [${badges.join(", ")}]`}</Text>
                  ) : null}
                  {isCurrent ? <Text color={SUCCESS}>{" ✓"}</Text> : null}
                </Box>
              );
            })}
          </Box>
        );
      })}
    </Pane>
  );
}

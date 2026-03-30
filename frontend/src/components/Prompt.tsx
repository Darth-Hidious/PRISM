import React, { useState, useRef } from "react";
import { Box, Text, useInput } from "ink";
import { PRIMARY, TEXT_MUTED, TEXT_DIM, BORDER, BORDER_ACTIVE } from "../theme.js";

interface Props {
  onSubmit: (text: string) => void;
  active?: boolean;
}

export function Prompt({ onSubmit, active = true }: Props) {
  const [display, setDisplay] = useState("");
  const bufferRef = useRef("");
  const cursorRef = useRef(0);
  const historyRef = useRef<string[]>([]);
  const historyIndexRef = useRef(-1);

  useInput(
    (input, key) => {
      if (!active) return;

      if (key.return) {
        const text = bufferRef.current.trim();
        if (text) {
          historyRef.current.unshift(text);
          if (historyRef.current.length > 100) historyRef.current.pop();
          onSubmit(text);
        }
        bufferRef.current = "";
        cursorRef.current = 0;
        historyIndexRef.current = -1;
        setDisplay("");
        return;
      }

      if (key.backspace || key.delete) {
        if (cursorRef.current > 0) {
          const buf = bufferRef.current;
          bufferRef.current =
            buf.slice(0, cursorRef.current - 1) + buf.slice(cursorRef.current);
          cursorRef.current--;
          setDisplay(bufferRef.current);
        }
        return;
      }

      if (key.leftArrow) {
        if (cursorRef.current > 0) cursorRef.current--;
        setDisplay(bufferRef.current); // force cursor redraw
        return;
      }

      if (key.rightArrow) {
        if (cursorRef.current < bufferRef.current.length) cursorRef.current++;
        setDisplay(bufferRef.current);
        return;
      }

      if (key.upArrow) {
        const hist = historyRef.current;
        if (hist.length > 0 && historyIndexRef.current < hist.length - 1) {
          historyIndexRef.current++;
          bufferRef.current = hist[historyIndexRef.current]!;
          cursorRef.current = bufferRef.current.length;
          setDisplay(bufferRef.current);
        }
        return;
      }

      if (key.downArrow) {
        if (historyIndexRef.current > 0) {
          historyIndexRef.current--;
          bufferRef.current = historyRef.current[historyIndexRef.current]!;
          cursorRef.current = bufferRef.current.length;
          setDisplay(bufferRef.current);
        } else if (historyIndexRef.current === 0) {
          historyIndexRef.current = -1;
          bufferRef.current = "";
          cursorRef.current = 0;
          setDisplay("");
        }
        return;
      }

      // Ignore control sequences
      if (key.ctrl || key.meta || key.escape) return;

      // Regular character input
      if (input && input.length > 0) {
        const buf = bufferRef.current;
        bufferRef.current =
          buf.slice(0, cursorRef.current) + input + buf.slice(cursorRef.current);
        cursorRef.current += input.length;
        setDisplay(bufferRef.current);
      }
    },
    { isActive: active },
  );

  // Render text with cursor
  const before = display.slice(0, cursorRef.current);
  const cursorChar = display[cursorRef.current] ?? " ";
  const after = display.slice(cursorRef.current + 1);

  return (
    <Box flexDirection="column" paddingLeft={1} marginTop={1}>
      <Box>
        <Text color={active ? PRIMARY : TEXT_DIM} bold>{"› "}</Text>
        {active ? (
          <Text>
            <Text>{before}</Text>
            <Text inverse>{cursorChar}</Text>
            <Text>{after}</Text>
          </Text>
        ) : (
          <Text color={TEXT_DIM}>...</Text>
        )}
      </Box>
      <Box paddingLeft={2}>
        <Text color={TEXT_DIM}>
          enter send · / command · ctrl+c exit
        </Text>
      </Box>
    </Box>
  );
}

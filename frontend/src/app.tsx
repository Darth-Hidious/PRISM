import React, { useState, useCallback } from "react";
import { Box, Static, Text } from "ink";
import { useBackend } from "./hooks/useBackend.js";
import { Welcome } from "./components/Welcome.js";
import { Prompt } from "./components/Prompt.js";
import { StreamingText } from "./components/StreamingText.js";
import { ToolCard } from "./components/ToolCard.js";
import { CostLine } from "./components/CostLine.js";
import { Spinner } from "./components/Spinner.js";
import { StatusLine } from "./components/StatusLine.js";
import { InputCard } from "./components/InputCard.js";
import { PlanCard } from "./components/PlanCard.js";
import { ApprovalPrompt } from "./components/ApprovalPrompt.js";
import { SessionList } from "./components/SessionList.js";
import { DIM, PRIMARY, MUTED, TEXT, WARNING } from "./theme.js";

interface HistoryItem {
  id: number;
  type: string;
  data: Record<string, any>;
}

interface Props {
  pythonPath: string;
  backendBin?: string;
  autoApprove?: boolean;
  resume?: string;
}

export function App({ pythonPath, backendBin, autoApprove, resume }: Props) {
  const { ready, events, sendMessage, sendCommand, sendPromptResponse } =
    useBackend(pythonPath, backendBin, autoApprove ?? false, resume);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [streamingText, setStreamingText] = useState("");
  const [spinnerVerb, setSpinnerVerb] = useState<string | null>(null);
  const [pendingApproval, setPendingApproval] = useState<{
    toolName: string;
    toolArgs: Record<string, any>;
  } | null>(null);
  const nextIdRef = React.useRef(0);
  const lastProcessedRef = React.useRef(0);
  const streamingRef = React.useRef("");

  // Helper to get a unique id and bump the counter
  const takeId = () => {
    const id = nextIdRef.current;
    nextIdRef.current += 1;
    return id;
  };

  // Process ALL events sequentially — never skip events between renders
  React.useEffect(() => {
    const start = lastProcessedRef.current;
    const end = events.length;
    if (start >= end) return;
    lastProcessedRef.current = end;

    let localText = streamingRef.current;
    let localSpinner: string | null = spinnerVerb;
    const newItems: HistoryItem[] = [];

    for (let i = start; i < end; i++) {
      const ev = events[i];
      switch (ev.method) {
        case "ui.welcome":
          newItems.push({ id: takeId(), type: "welcome", data: ev.params });
          break;
        case "ui.text.delta":
          localText += ev.params.text;
          break;
        case "ui.text.flush":
          if (ev.params.text.trim()) {
            newItems.push({ id: takeId(), type: "text", data: { text: ev.params.text } });
          }
          localText = "";
          break;
        case "ui.tool.start":
          localSpinner = ev.params.verb;
          setPendingApproval(null);
          break;
        case "ui.card":
          localSpinner = null;
          if (ev.params.card_type === "plan") {
            newItems.push({ id: takeId(), type: "plan", data: ev.params });
          } else {
            newItems.push({ id: takeId(), type: "card", data: ev.params });
          }
          break;
        case "ui.cost":
          newItems.push({ id: takeId(), type: "cost", data: ev.params });
          break;
        case "ui.turn.complete":
          localSpinner = null;
          if (localText.trim()) {
            newItems.push({ id: takeId(), type: "text", data: { text: localText } });
          }
          localText = "";
          break;
        case "ui.status":
          newItems.push({ id: takeId(), type: "status", data: ev.params });
          break;
        case "ui.prompt":
          if (ev.params.prompt_type === "approval") {
            newItems.push({ id: takeId(), type: "approval", data: ev.params });
            setPendingApproval({
              toolName: ev.params.tool_name,
              toolArgs: ev.params.tool_args,
            });
          }
          break;
        case "ui.session.list":
          newItems.push({ id: takeId(), type: "sessions", data: ev.params });
          break;
      }
    }

    // Batch all state updates into a single render
    if (newItems.length > 0) {
      setHistory((h) => [...h, ...newItems]);
    }
    streamingRef.current = localText;
    setStreamingText(localText);
    setSpinnerVerb(localSpinner);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events.length]);

  const handleApprovalResponse = useCallback(
    (response: string) => {
      sendPromptResponse("approval", response);
      setPendingApproval(null);
    },
    [sendPromptResponse],
  );

  const handleSubmit = useCallback(
    (text: string) => {
      if (text.startsWith("/")) {
        sendCommand(text);
      } else {
        setHistory((h) => [
          ...h,
          { id: takeId(), type: "input", data: { text } },
        ]);
        sendMessage(text);
      }
    },
    [sendMessage, sendCommand],
  );

  if (!ready) return <Spinner verb="Starting PRISM..." />;

  return (
    <Box flexDirection="column" paddingX={1}>
      <Box justifyContent="space-between" marginBottom={1}>
        <Text>
          <Text color={PRIMARY} bold>PRISM</Text>
          <Text color={MUTED}>  coding shell</Text>
        </Text>
        <Text color={DIM}>
          {pendingApproval ? (
            <Text color={WARNING}>approval pending</Text>
          ) : spinnerVerb ? (
            "working"
          ) : (
            <Text color={TEXT}>ready</Text>
          )}
        </Text>
      </Box>

      <Static items={history}>
        {(item) => (
          <HistoryRenderer
            key={item.id}
            item={item}
            sendPromptResponse={sendPromptResponse}
          />
        )}
      </Static>

      {streamingText ? <StreamingText text={streamingText} streaming /> : null}
      {spinnerVerb ? <Spinner verb={spinnerVerb} /> : null}

      {pendingApproval ? (
        <ApprovalPrompt
          toolName={pendingApproval.toolName}
          toolArgs={pendingApproval.toolArgs}
          onResponse={handleApprovalResponse}
        />
      ) : (
        <Prompt
          onSubmit={handleSubmit}
          active={!streamingText && !spinnerVerb}
        />
      )}
    </Box>
  );
}

function HistoryRenderer({
  item,
  sendPromptResponse,
}: {
  item: HistoryItem;
  sendPromptResponse: (type: string, response: string) => void;
}) {
  switch (item.type) {
    case "welcome":
      return (
        <Welcome
          version={item.data.version}
          status={item.data.status}
          autoApprove={item.data.auto_approve}
        />
      );
    case "input":
      return <InputCard text={item.data.text} />;
    case "text":
      return <StreamingText text={item.data.text} />;
    case "card":
      return (
        <ToolCard
          cardType={item.data.card_type}
          toolName={item.data.tool_name}
          elapsedMs={item.data.elapsed_ms}
          content={item.data.content}
          data={item.data.data}
        />
      );
    case "plan":
      return <PlanCard content={item.data.content} />;
    case "cost":
      return (
        <CostLine
          inputTokens={item.data.input_tokens}
          outputTokens={item.data.output_tokens}
          turnCost={item.data.turn_cost}
          sessionCost={item.data.session_cost}
        />
      );
    case "status":
      return (
        <StatusLine
          autoApprove={item.data.auto_approve}
          messageCount={item.data.message_count}
          hasPlan={item.data.has_plan}
        />
      );
    case "approval":
      // Approval is rendered as live interactive element at the bottom,
      // not in Static history. Show a placeholder in scroll-back.
      return null;
    case "sessions":
      return <SessionList sessions={item.data.sessions} />;
    default:
      return null;
  }
}

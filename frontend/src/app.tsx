import React, { useState, useCallback } from "react";
import { Box, Static } from "ink";
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

interface HistoryItem {
  id: number;
  type: string;
  data: Record<string, any>;
}

interface Props {
  pythonPath: string;
  autoApprove?: boolean;
}

export function App({ pythonPath, autoApprove }: Props) {
  const { ready, events, sendMessage, sendCommand, sendPromptResponse } =
    useBackend(pythonPath);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [streamingText, setStreamingText] = useState("");
  const [spinnerVerb, setSpinnerVerb] = useState<string | null>(null);
  const nextIdRef = React.useRef(0);

  // Helper to get a unique id and bump the counter
  const takeId = () => {
    const id = nextIdRef.current;
    nextIdRef.current += 1;
    return id;
  };

  // Process events into history + live state
  React.useEffect(() => {
    if (events.length === 0) return;
    const latest = events[events.length - 1];

    switch (latest.method) {
      case "ui.welcome":
        setHistory((h) => [...h, { id: takeId(), type: "welcome", data: latest.params }]);
        break;
      case "ui.text.delta":
        setStreamingText((t) => t + latest.params.text);
        break;
      case "ui.text.flush":
        if (latest.params.text.trim()) {
          setHistory((h) => [
            ...h,
            { id: takeId(), type: "text", data: { text: latest.params.text } },
          ]);
        }
        setStreamingText("");
        break;
      case "ui.tool.start":
        setSpinnerVerb(latest.params.verb);
        break;
      case "ui.card":
        setSpinnerVerb(null);
        if (latest.params.card_type === "plan") {
          setHistory((h) => [...h, { id: takeId(), type: "plan", data: latest.params }]);
        } else {
          setHistory((h) => [...h, { id: takeId(), type: "card", data: latest.params }]);
        }
        break;
      case "ui.cost":
        setHistory((h) => [...h, { id: takeId(), type: "cost", data: latest.params }]);
        break;
      case "ui.turn.complete":
        setSpinnerVerb(null);
        setStreamingText((current) => {
          if (current.trim()) {
            setHistory((h) => [
              ...h,
              { id: takeId(), type: "text", data: { text: current } },
            ]);
          }
          return "";
        });
        break;
      case "ui.status":
        setHistory((h) => [...h, { id: takeId(), type: "status", data: latest.params }]);
        break;
      case "ui.prompt":
        if (latest.params.prompt_type === "approval") {
          setHistory((h) => [
            ...h,
            { id: takeId(), type: "approval", data: latest.params },
          ]);
        }
        break;
      case "ui.session.list":
        setHistory((h) => [
          ...h,
          { id: takeId(), type: "sessions", data: latest.params },
        ]);
        break;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events.length]);

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
    <Box flexDirection="column">
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

      <Prompt onSubmit={handleSubmit} />
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
      return (
        <ApprovalPrompt
          toolName={item.data.tool_name}
          toolArgs={item.data.tool_args}
          onResponse={(r) => sendPromptResponse("approval", r)}
        />
      );
    case "sessions":
      return <SessionList sessions={item.data.sessions} />;
    default:
      return null;
  }
}

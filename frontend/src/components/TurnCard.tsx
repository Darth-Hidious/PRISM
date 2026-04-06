import React from "react";
import { Box, Text } from "ink";
import InkSpinner from "ink-spinner";
import {
  BORDER_AGENT,
  BORDER_USER,
  PRIMARY,
  SECONDARY,
  TEXT,
  TEXT_DIM,
  TEXT_MUTED,
  WARNING,
} from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";
import { ToolCard } from "./ToolCard.js";
import { PlanCard } from "./PlanCard.js";
import { CostLine } from "./CostLine.js";
import { ApprovalPrompt } from "./ApprovalPrompt.js";
import { SessionList } from "./SessionList.js";

export interface TurnInput {
  text: string;
  kind: "prompt" | "command";
}

export interface TurnToolCall {
  callId?: string;
  toolName: string;
  cardType: string;
  elapsedMs: number;
  content: string;
  data: Record<string, any>;
  pending?: boolean;
  verb?: string;
}

export interface TurnPlanCard {
  content: string;
}

export interface TurnSessionSummary {
  session_id: string;
  created_at: number;
  turn_count: number;
  model: string;
  size_kb: number;
  is_latest: boolean;
}

export interface TurnCommandViewTab {
  id: string;
  title: string;
  body: string;
  tone?: string;
}

export interface TurnCommandView {
  title: string;
  body?: string;
  tone?: string;
  tabs?: TurnCommandViewTab[];
  selectedTab?: string;
  footer?: string;
}

export interface TurnCost {
  inputTokens: number;
  outputTokens: number;
  turnCost?: number;
  sessionCost?: number;
}

export interface TurnApprovalRequest {
  toolName: string;
  toolArgs: Record<string, any>;
  toolDescription?: string;
  requiresApproval?: boolean;
  permissionMode?: string;
}

export interface TurnData {
  id: number;
  input?: TurnInput;
  assistantText?: string;
  toolCalls: TurnToolCall[];
  planCards: TurnPlanCard[];
  sessionList?: TurnSessionSummary[];
  cost?: TurnCost;
  viewSummary?: string;
  approvalRequest?: TurnApprovalRequest;
  commandView?: TurnCommandView;
}

interface Props {
  turn: TurnData;
  streaming?: boolean;
  onApprovalResponse?: (response: string) => void;
}

export function turnHasVisibleContent(turn: TurnData): boolean {
  return !!(
    turn.input ||
    turn.assistantText ||
    turn.toolCalls.length > 0 ||
    turn.planCards.length > 0 ||
    (turn.sessionList?.length ?? 0) > 0 ||
    turn.cost ||
    turn.viewSummary ||
    turn.approvalRequest ||
    turn.commandView
  );
}

export function cloneTurn(turn: TurnData): TurnData {
  // Event folding mutates a local working copy before committing state. Clone
  // nested arrays so a live streaming turn never mutates finalized history.
  return {
    ...turn,
    input: turn.input ? { ...turn.input } : undefined,
    toolCalls: turn.toolCalls.map((toolCall) => ({ ...toolCall })),
    planCards: turn.planCards.map((card) => ({ ...card })),
    sessionList: turn.sessionList?.map((session) => ({ ...session })),
    cost: turn.cost ? { ...turn.cost } : undefined,
    approvalRequest: turn.approvalRequest
      ? {
          ...turn.approvalRequest,
          toolArgs: { ...turn.approvalRequest.toolArgs },
        }
      : undefined,
    commandView: turn.commandView
      ? {
          ...turn.commandView,
          tabs: turn.commandView.tabs?.map((tab) => ({ ...tab })),
        }
      : undefined,
  };
}

function TurnInputHeader({ input }: { input: TurnInput }) {
  const isCommand = input.kind === "command";
  const label = isCommand ? "you · command" : "you · prompt";
  const prefix = isCommand ? "/" : "›";

  return (
    <Box
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={BORDER_USER}
      paddingLeft={2}
      marginTop={1}
      flexDirection="column"
    >
      <Text color={TEXT_DIM}>{label}</Text>
      <Text color={isCommand ? WARNING : TEXT} bold>
        {prefix} {input.text}
      </Text>
    </Box>
  );
}

function ThinkingRow({ verb }: { verb?: string }) {
  return (
    <Box marginTop={1}>
      <Text color={PRIMARY}>
        <InkSpinner type="dots" />
      </Text>
      <Text color={TEXT_MUTED}>{` ${verb ?? "thinking"}`}</Text>
    </Box>
  );
}

function CommandViewPreview({ view }: { view: TurnCommandView }) {
  const activeTab =
    view.tabs?.find((tab) => tab.id === view.selectedTab) ?? view.tabs?.[0];
  const body = activeTab?.body ?? view.body ?? "";

  return (
    <Box
      flexDirection="column"
      borderStyle="single"
      borderLeft
      borderRight={false}
      borderTop={false}
      borderBottom={false}
      borderColor={PRIMARY}
      paddingLeft={2}
      marginTop={1}
    >
      <Text color={PRIMARY} bold>
        {view.title}
        {activeTab ? <Text color={TEXT_DIM}>{` · ${activeTab.title}`}</Text> : null}
      </Text>
      {body ? (
        <Box marginTop={0} flexDirection="column">
          <MarkdownText text={body} />
        </Box>
      ) : null}
      {view.footer ? (
        <Text color={TEXT_DIM}>{view.footer}</Text>
      ) : null}
    </Box>
  );
}

export function TurnCard({ turn, streaming = false, onApprovalResponse }: Props) {
  const hasAssistantBody = !!(
    turn.assistantText ||
    turn.toolCalls.length > 0 ||
    turn.planCards.length > 0 ||
    (turn.sessionList?.length ?? 0) > 0 ||
    turn.cost ||
    turn.viewSummary ||
    turn.approvalRequest ||
    turn.commandView ||
    streaming
  );
  const pendingTool = turn.toolCalls.find((toolCall) => toolCall.pending);
  const hasActivity = turn.toolCalls.length > 0 || turn.approvalRequest;
  const assistantLabel = turn.approvalRequest
    ? "prism · approval"
    : pendingTool
      ? "prism · tools"
      : turn.commandView
        ? "prism · command"
        : streaming
          ? "prism · working"
          : "prism";

  return (
    <Box flexDirection="column">
      {turn.input ? <TurnInputHeader input={turn.input} /> : null}

      {hasAssistantBody ? (
        <Box
          borderStyle="single"
          borderLeft
          borderRight={false}
          borderTop={false}
          borderBottom={false}
          borderColor={BORDER_AGENT}
          paddingLeft={2}
          marginTop={turn.input ? 0 : 1}
          flexDirection="column"
        >
          <Text color={SECONDARY}>{assistantLabel}</Text>

          {turn.assistantText ? (
            <Box marginTop={0} flexDirection="column">
              {streaming ? (
                <Text color={TEXT}>{turn.assistantText}</Text>
              ) : (
                <MarkdownText text={turn.assistantText} />
              )}
            </Box>
          ) : null}

          {turn.viewSummary ? (
            <Box marginTop={turn.assistantText ? 1 : 0}>
              <Text color={TEXT_MUTED}>{turn.viewSummary}</Text>
            </Box>
          ) : null}

          {turn.commandView ? <CommandViewPreview view={turn.commandView} /> : null}

          {hasActivity ? (
            <Box marginTop={turn.assistantText || turn.viewSummary ? 1 : 0} flexDirection="column">
              <Text color={TEXT_DIM}>activity</Text>

              {turn.toolCalls.map((toolCall) => (
                <ToolCard
                  key={toolCall.callId ?? `${toolCall.toolName}-${toolCall.elapsedMs}-${toolCall.content}`}
                  cardType={toolCall.cardType}
                  toolName={toolCall.toolName}
                  elapsedMs={toolCall.elapsedMs}
                  content={toolCall.content}
                  data={toolCall.data}
                  pending={toolCall.pending}
                />
              ))}

              {turn.approvalRequest && onApprovalResponse ? (
                // Approval lives inside the active turn so tool gating reads as
                // part of the agent's step, not as a detached footer dialog.
              <ApprovalPrompt
                toolName={turn.approvalRequest.toolName}
                toolArgs={turn.approvalRequest.toolArgs}
                toolDescription={turn.approvalRequest.toolDescription}
                requiresApproval={turn.approvalRequest.requiresApproval}
                permissionMode={turn.approvalRequest.permissionMode}
                onResponse={onApprovalResponse}
              />
              ) : null}
            </Box>
          ) : null}

          {turn.planCards.map((card, index) => (
            <PlanCard key={`${turn.id}-plan-${index}`} content={card.content} />
          ))}

          {turn.sessionList ? <SessionList sessions={turn.sessionList} /> : null}

          {turn.cost ? (
            <CostLine
              inputTokens={turn.cost.inputTokens}
              outputTokens={turn.cost.outputTokens}
              turnCost={turn.cost.turnCost}
              sessionCost={turn.cost.sessionCost}
            />
          ) : null}

          {streaming && !pendingTool ? (
            <ThinkingRow />
          ) : null}

          {streaming && pendingTool?.verb ? (
            <Box marginTop={1}>
              {/* Keep the active tool verb visible even when the row itself is
                  still in a compact pending state. */}
              <Text color={TEXT_DIM}>{pendingTool.verb}</Text>
            </Box>
          ) : null}
        </Box>
      ) : null}
    </Box>
  );
}

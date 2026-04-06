import React, { useState, useCallback } from "react";
import { Box, Static } from "ink";
import { useBackend } from "./hooks/useBackend.js";
import { Welcome } from "./components/Welcome.js";
import { Prompt } from "./components/Prompt.js";
import { Spinner } from "./components/Spinner.js";
import { StatusLine } from "./components/StatusLine.js";
import { SessionList } from "./components/SessionList.js";
import { ModelSelector } from "./components/ModelSelector.js";
import { CommandView } from "./components/CommandView.js";
import {
  PermissionEditor,
  type PermissionTool,
} from "./components/PermissionEditor.js";
import { SessionPicker } from "./components/SessionPicker.js";
import {
  cloneTurn,
  TurnCard,
  type TurnCommandView,
  type TurnSessionSummary,
  turnHasVisibleContent,
  type TurnCost,
  type TurnData,
  type TurnInput,
  type TurnToolCall,
} from "./components/TurnCard.js";

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

interface StatusState {
  autoApprove: boolean;
  messageCount: number;
  hasPlan: boolean;
  sessionMode?: string;
  planStatus?: string;
  model?: string;
  projectRoot?: string;
}

interface WelcomeState {
  sessionId?: string;
  toolCount?: number;
  resumed?: boolean;
}

interface ActiveView {
  viewType: string;
  title: string;
  body?: string;
  tone?: string;
  tabs?: Array<{
    id: string;
    title: string;
    body: string;
    tone?: string;
  }>;
  selectedTab?: string;
  footer?: string;
}

interface ActivePermissions {
  mode: string;
  autoApproved: PermissionTool[];
  blocked: PermissionTool[];
  approvalRequired: PermissionTool[];
  readOnly: PermissionTool[];
  workspaceWrite: PermissionTool[];
  fullAccess: PermissionTool[];
  allowOverrides: string[];
  denyOverrides: string[];
  notice?: string;
}

function toTurnCommandView(view: ActiveView): TurnCommandView {
  return {
    title: view.title,
    body: view.body,
    tone: view.tone,
    tabs: view.tabs?.map((tab) => ({ ...tab })),
    selectedTab: view.selectedTab,
    footer: view.footer,
  };
}

export function App({ pythonPath, backendBin, autoApprove, resume }: Props) {
  const { ready, events, sendMessage, sendCommand, sendPromptResponse, sendModelSelect } =
    useBackend(pythonPath, backendBin, autoApprove ?? false, resume);
  const [history, setHistory] = useState<HistoryItem[]>([]);
  const [draftTurn, setDraftTurn] = useState<TurnData | null>(null);
  const [modelList, setModelList] = useState<{
    current: string;
    models: any[];
  } | null>(null);
  const [statusState, setStatusState] = useState<StatusState | null>(null);
  const [welcomeState, setWelcomeState] = useState<WelcomeState | null>(null);
  const [activeView, setActiveView] = useState<ActiveView | null>(null);
  const [activePermissions, setActivePermissions] = useState<ActivePermissions | null>(null);
  const [activeSessionPicker, setActiveSessionPicker] = useState<TurnSessionSummary[] | null>(
    null,
  );
  const nextIdRef = React.useRef(0);
  const lastProcessedRef = React.useRef(0);
  const historyRef = React.useRef<HistoryItem[]>([]);
  const draftTurnRef = React.useRef<TurnData | null>(null);

  const takeId = () => {
    const id = nextIdRef.current;
    nextIdRef.current += 1;
    return id;
  };

  React.useEffect(() => {
    historyRef.current = history;
  }, [history]);

  React.useEffect(() => {
    draftTurnRef.current = draftTurn;
  }, [draftTurn]);

  const createDraftTurn = useCallback(
    (input?: TurnInput): TurnData => ({
      id: takeId(),
      input,
      toolCalls: [],
      planCards: [],
    }),
    [],
  );

  const finalizeDraftTurn = useCallback(
    (items: HistoryItem[], turn: TurnData | null): TurnData | null => {
      if (!turn || !turnHasVisibleContent(turn)) {
        return null;
      }
      items.push({ id: turn!.id, type: "turn", data: turn! });
      return null;
    },
    [],
  );

  // Process events sequentially
  React.useEffect(() => {
    const start = lastProcessedRef.current;
    const end = events.length;
    if (start >= end) return;
    lastProcessedRef.current = end;

    let localHistory = historyRef.current.slice();
    let localDraft = draftTurnRef.current ? cloneTurn(draftTurnRef.current) : null;
    let historyChanged = false;
    let draftChanged = false;

    const ensureDraftTurn = () => {
      if (!localDraft) {
        // Resume flows and command helpers can emit UI events before the user
        // types again. We still fold that output into a normal assistant turn.
        localDraft = createDraftTurn();
        draftChanged = true;
      }
      return localDraft;
    };

    const appendAssistantText = (text: string) => {
      if (!text) return;
      const turn = ensureDraftTurn();
      turn.assistantText = (turn.assistantText ?? "") + text;
      draftChanged = true;
    };

    const upsertToolCall = (toolCall: TurnToolCall) => {
      const turn = ensureDraftTurn();
      const matchIndex = turn.toolCalls.findIndex(
        (existing) => existing.callId && existing.callId === toolCall.callId,
      );

      // Tool start/result events share the same call ID. Updating in place keeps
      // a single row for the tool turn instead of rendering separate transport events.
      if (matchIndex >= 0) {
        turn.toolCalls[matchIndex] = { ...turn.toolCalls[matchIndex]!, ...toolCall };
      } else {
        turn.toolCalls.push(toolCall);
      }
      draftChanged = true;
    };

    for (let i = start; i < end; i++) {
      const ev = events[i];
      switch (ev.method) {
        case "ui.welcome":
          setWelcomeState({
            sessionId: ev.params.session_id ? String(ev.params.session_id) : undefined,
            toolCount:
              ev.params.tool_count !== undefined ? Number(ev.params.tool_count) : undefined,
            resumed: !!ev.params.resumed,
          });
          localHistory.push({ id: takeId(), type: "welcome", data: ev.params });
          historyChanged = true;
          break;
        case "ui.text.delta":
          appendAssistantText(String(ev.params.text ?? ""));
          break;
        case "ui.text.flush":
          appendAssistantText(String(ev.params.text ?? ""));
          break;
        case "ui.tool.start":
          if (localDraft?.approvalRequest) {
            localDraft.approvalRequest = undefined;
            draftChanged = true;
          }
          upsertToolCall({
            callId: ev.params.call_id ? String(ev.params.call_id) : undefined,
            toolName: String(ev.params.tool_name ?? "tool"),
            cardType: "results",
            elapsedMs: 0,
            content: "",
            data: ev.params.preview
              ? { summary: String(ev.params.preview), preview: String(ev.params.preview) }
              : {},
            pending: true,
            verb: ev.params.verb ? String(ev.params.verb) : undefined,
          });
          break;
        case "ui.card":
          if (ev.params.card_type === "plan") {
            const turn = ensureDraftTurn();
            // Plan cards belong to the same turn as the surrounding text/tool
            // output so plan-mode replies read as one coherent assistant step.
            turn.planCards.push({ content: String(ev.params.content ?? "") });
            draftChanged = true;
          } else {
            upsertToolCall({
              callId: ev.params.data?.call_id ? String(ev.params.data.call_id) : undefined,
              toolName: String(ev.params.tool_name ?? "tool"),
              cardType: String(ev.params.card_type ?? "results"),
              elapsedMs: Number(ev.params.elapsed_ms ?? 0),
              content: String(ev.params.content ?? ""),
              data:
                ev.params.data && typeof ev.params.data === "object"
                  ? (ev.params.data as Record<string, any>)
                  : {},
              pending: false,
            });
          }
          break;
        case "ui.cost":
          if (localDraft) {
            localDraft.cost = {
              inputTokens: Number(ev.params.input_tokens ?? 0),
              outputTokens: Number(ev.params.output_tokens ?? 0),
              turnCost:
                ev.params.turn_cost !== undefined ? Number(ev.params.turn_cost) : undefined,
              sessionCost:
                ev.params.session_cost !== undefined
                  ? Number(ev.params.session_cost)
                  : undefined,
            } satisfies TurnCost;
            draftChanged = true;
          }
          break;
        case "ui.turn.complete":
          localDraft = finalizeDraftTurn(localHistory, localDraft);
          historyChanged = true;
          draftChanged = true;
          break;
        case "ui.status":
          setStatusState({
            autoApprove: !!ev.params.auto_approve,
            messageCount: Number(ev.params.message_count ?? 0),
            hasPlan: !!ev.params.has_plan,
            sessionMode: ev.params.session_mode ? String(ev.params.session_mode) : undefined,
            planStatus: ev.params.plan_status ? String(ev.params.plan_status) : undefined,
            model: ev.params.model ? String(ev.params.model) : undefined,
            projectRoot: ev.params.project_root
              ? String(ev.params.project_root)
              : undefined,
          });
          break;
        case "ui.prompt":
          if (ev.params.prompt_type === "approval") {
            const turn = ensureDraftTurn();
            // Approval prompts are folded into the active turn so the user sees
            // them in the same transcript block as the tool activity they gate.
            turn.approvalRequest = {
              toolName: String(ev.params.tool_name ?? "tool"),
              toolArgs:
                ev.params.tool_args && typeof ev.params.tool_args === "object"
                  ? (ev.params.tool_args as Record<string, any>)
                  : {},
              toolDescription: ev.params.tool_description
                ? String(ev.params.tool_description)
                : undefined,
              requiresApproval:
                ev.params.requires_approval !== undefined
                  ? !!ev.params.requires_approval
                  : undefined,
              permissionMode: ev.params.permission_mode
                ? String(ev.params.permission_mode)
                : undefined,
            };
            draftChanged = true;
          }
          break;
        case "ui.session.list":
          if (localDraft?.input?.kind === "command") {
            // `/sessions` belongs in the current command turn, not as a
            // detached history block that loses the command context.
            localDraft.sessionList = Array.isArray(ev.params.sessions)
              ? ev.params.sessions.map((session: any) => ({
                  session_id: String(session.session_id ?? ""),
                  created_at: Number(session.created_at ?? 0),
                  turn_count: Number(session.turn_count ?? 0),
                  model: String(session.model ?? ""),
                  size_kb: Number(session.size_kb ?? 0),
                  is_latest: !!session.is_latest,
                })) satisfies TurnSessionSummary[]
              : [];
            localDraft.viewSummary = `Listed ${localDraft.sessionList.length} sessions`;
            setActiveSessionPicker(localDraft.sessionList ?? []);
            setActiveView(null);
            setActivePermissions(null);
            draftChanged = true;
          } else {
            localHistory.push({ id: takeId(), type: "sessions", data: ev.params });
            historyChanged = true;
          }
          break;
        case "ui.permissions":
          setActivePermissions({
            mode: String(ev.params.mode ?? "chat"),
            autoApproved: Array.isArray(ev.params.auto_approved)
              ? ev.params.auto_approved.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            blocked: Array.isArray(ev.params.blocked)
              ? ev.params.blocked.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            approvalRequired: Array.isArray(ev.params.approval_required)
              ? ev.params.approval_required.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            readOnly: Array.isArray(ev.params.read_only)
              ? ev.params.read_only.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            workspaceWrite: Array.isArray(ev.params.workspace_write)
              ? ev.params.workspace_write.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            fullAccess: Array.isArray(ev.params.full_access)
              ? ev.params.full_access.map((tool: any) => ({
                  name: String(tool.name ?? ""),
                  permission_mode: String(tool.permission_mode ?? ""),
                  requires_approval: !!tool.requires_approval,
                  description: String(tool.description ?? ""),
                  current_behavior: String(tool.current_behavior ?? ""),
                }))
              : [],
            allowOverrides: Array.isArray(ev.params.allow_overrides)
              ? ev.params.allow_overrides.map(String)
              : [],
            denyOverrides: Array.isArray(ev.params.deny_overrides)
              ? ev.params.deny_overrides.map(String)
              : [],
            notice: ev.params.notice ? String(ev.params.notice) : undefined,
          });
          setActiveView(null);
          if (localDraft?.input?.kind === "command") {
            localDraft.viewSummary = "Opened Permissions";
            draftChanged = true;
          }
          break;
        case "ui.model.list":
          setModelList({ current: ev.params.current, models: ev.params.models });
          setActiveView(null);
          setActivePermissions(null);
          setActiveSessionPicker(null);
          break;
        case "ui.view":
          const nextView = {
            viewType: String(ev.params.view_type),
            title: String(ev.params.title),
            body: ev.params.body ? String(ev.params.body) : undefined,
            tone: ev.params.tone ? String(ev.params.tone) : undefined,
            tabs: Array.isArray(ev.params.tabs)
              ? ev.params.tabs.map((tab: any) => ({
                  id: String(tab.id),
                  title: String(tab.title),
                  body: String(tab.body),
                  tone: tab.tone ? String(tab.tone) : undefined,
                }))
              : undefined,
            selectedTab: ev.params.selected_tab
              ? String(ev.params.selected_tab)
              : undefined,
            footer: ev.params.footer ? String(ev.params.footer) : undefined,
          } satisfies ActiveView;
          setActiveView(nextView);
          setActivePermissions(null);
          setActiveSessionPicker(null);
          if (localDraft?.input?.kind === "command") {
            // Keep a compact transcript breadcrumb for slash commands even when
            // the rich body is shown in the modal-style command view.
            const selectedTab = Array.isArray(ev.params.tabs)
              ? ev.params.tabs.find((tab: any) => String(tab.id) === String(ev.params.selected_tab))
              : null;
            localDraft.viewSummary = selectedTab
              ? `Opened ${String(ev.params.title)} · ${String(selectedTab.title)}`
              : `Opened ${String(ev.params.title)}`;
            // Command results should remain visible in the transcript after the
            // modal closes, otherwise slash commands feel detached from the turn.
            localDraft.commandView = toTurnCommandView(nextView);
            draftChanged = true;
          }
          break;
      }
    }

    // The backend speaks in low-level transport events. Folding them into a
    // draft turn here makes the TUI render like a conversation instead of a log.
    if (historyChanged) {
      historyRef.current = localHistory;
      setHistory(localHistory);
    }
    if (draftChanged) {
      draftTurnRef.current = localDraft;
      setDraftTurn(localDraft);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [events.length]);

  const handleApprovalResponse = useCallback(
    (response: string) => {
      sendPromptResponse("approval", response, draftTurn?.approvalRequest?.toolName);
      setDraftTurn((current) =>
        current ? { ...current, approvalRequest: undefined } : current,
      );
      draftTurnRef.current = draftTurnRef.current
        ? { ...draftTurnRef.current, approvalRequest: undefined }
        : null;
    },
    [draftTurn?.approvalRequest?.toolName, sendPromptResponse],
  );

  const dispatchInput = useCallback(
    (text: string) => {
      const input: TurnInput = {
        text,
        kind: text.startsWith("/") ? "command" : "prompt",
      };

      const nextDraft = createDraftTurn(input);
      // The ref is read by the event-folding effect before React may commit
      // state, so both need the same draft instance immediately.
      draftTurnRef.current = nextDraft;
      setDraftTurn(nextDraft);
      setActiveView(null);
      setActivePermissions(null);
      setActiveSessionPicker(null);

      if (text.startsWith("/")) {
        sendCommand(text);
      } else {
        sendMessage(text);
      }
    },
    [createDraftTurn, sendMessage, sendCommand],
  );

  const handleSubmit = useCallback((text: string) => {
    dispatchInput(text);
  }, [dispatchInput]);

  const activeOverlayTitle = modelList
    ? "Model"
    : activePermissions
      ? "Permissions"
      : activeSessionPicker
        ? "Sessions"
        : activeView?.title;

  if (!ready) return <Spinner verb="Starting PRISM..." />;

  return (
    <Box flexDirection="column" paddingX={1}>
      <Static items={history}>
        {(item) => <HistoryRenderer key={item.id} item={item} />}
      </Static>

      {draftTurn ? (
        <TurnCard
          turn={draftTurn}
          streaming
          onApprovalResponse={handleApprovalResponse}
        />
      ) : null}
      {activePermissions ? (
        <PermissionEditor
          mode={activePermissions.mode}
          autoApproved={activePermissions.autoApproved}
          blocked={activePermissions.blocked}
          approvalRequired={activePermissions.approvalRequired}
          readOnly={activePermissions.readOnly}
          workspaceWrite={activePermissions.workspaceWrite}
          fullAccess={activePermissions.fullAccess}
          allowOverrides={activePermissions.allowOverrides}
          denyOverrides={activePermissions.denyOverrides}
          notice={activePermissions.notice}
          onCommand={(command) => sendCommand(command, { silent: true })}
          onClose={() => setActivePermissions(null)}
        />
      ) : activeSessionPicker ? (
        <SessionPicker
          sessions={activeSessionPicker}
          onResume={(sessionId) => dispatchInput(`/resume ${sessionId}`)}
          onClose={() => setActiveSessionPicker(null)}
        />
      ) : activeView ? (
        <CommandView
          title={activeView.title}
          body={activeView.body}
          tone={activeView.tone}
          tabs={activeView.tabs}
          selectedTab={activeView.selectedTab}
          footer={activeView.footer}
          onClose={() => setActiveView(null)}
        />
      ) : null}
      {statusState ? (
        <StatusLine
          autoApprove={statusState.autoApprove}
          messageCount={statusState.messageCount}
          hasPlan={statusState.hasPlan}
          sessionMode={statusState.sessionMode}
          planStatus={statusState.planStatus}
          sessionId={welcomeState?.sessionId}
          toolCount={welcomeState?.toolCount}
          resumed={welcomeState?.resumed}
          approvalPending={!!draftTurn?.approvalRequest}
          turnActive={!!draftTurn}
          activeViewTitle={activeOverlayTitle}
          model={statusState.model}
          projectRoot={statusState.projectRoot}
        />
      ) : null}

      {modelList ? (
        <ModelSelector
          current={modelList.current}
          models={modelList.models}
          onSelect={(id) => {
            sendModelSelect(id);
            setModelList(null);
          }}
          onCancel={() => setModelList(null)}
        />
      ) : (
        activeView || activePermissions || activeSessionPicker ? null : (
          <Prompt
            onSubmit={handleSubmit}
            active={!draftTurn}
          />
        )
      )}
    </Box>
  );
}

function HistoryRenderer({ item }: { item: HistoryItem }) {
  switch (item.type) {
    case "welcome":
      return (
        <Welcome
          version={item.data.version}
          status={item.data.status}
          autoApprove={item.data.auto_approve}
          toolCount={item.data.tool_count}
          sessionId={item.data.session_id}
          resumed={item.data.resumed}
          resumedMessages={item.data.resumed_messages}
        />
      );
    case "turn":
      return <TurnCard turn={item.data as TurnData} />;
    case "sessions":
      return <SessionList sessions={item.data.sessions} />;
    default:
      return null;
  }
}

import React from "react";
import { Box, Text } from "ink";
import {
  SUCCESS, ERROR, TEXT, TEXT_MUTED, TEXT_DIM,
  BORDER, BG_PANEL, BORDER_AGENT,
} from "../theme.js";
import { MarkdownText } from "./MarkdownText.js";
import { Pane } from "./chrome/Pane.js";

interface Props {
  cardType: string;
  toolName: string;
  elapsedMs: number;
  content: string;
  data: Record<string, any>;
  pending?: boolean;
}

function previewValue(value: unknown, limit = 100): string {
  const text = typeof value === "string" ? value : JSON.stringify(value);
  if (!text) return "";
  return text.length > limit ? `${text.slice(0, limit)}...` : text;
}

function renderStructuredCommandData(data: Record<string, any>) {
  const root = typeof data?.root === "string" ? data.root : "";
  const parsed = data?.parsed_stdout;
  if (!root || !parsed) return null;

  if (root === "models") {
    const models = Array.isArray(parsed) ? parsed : [parsed];
    if (!models.length) return null;
    return (
      <Box marginTop={1} flexDirection="column">
        <Text color={TEXT_DIM}>{`models: ${models.length}`}</Text>
        {models.slice(0, 6).map((model: any, index: number) => {
          const modelId = String(model?.model_id ?? model?.id ?? "?");
          const display = String(model?.display_name ?? model?.name ?? modelId);
          const provider = String(model?.provider ?? "?");
          const ctx = model?.context_window ? ` ctx=${model.context_window}` : "";
          return (
            <Text key={`${modelId}-${index}`} color={TEXT_MUTED}>
              {`${provider} · ${display}${ctx}`}
            </Text>
          );
        })}
        {models.length > 6 ? (
          <Text color={TEXT_DIM}>{`+${models.length - 6} more`}</Text>
        ) : null}
      </Box>
    );
  }

  if (root === "deploy") {
    const deployments = Array.isArray(parsed) ? parsed : null;
    if (deployments) {
      return (
        <Box marginTop={1} flexDirection="column">
          <Text color={TEXT_DIM}>{`deployments: ${deployments.length}`}</Text>
          {deployments.slice(0, 5).map((deployment: any, index: number) => {
            const name = String(deployment?.name ?? deployment?.deployment_id ?? deployment?.id ?? "?");
            const status = String(deployment?.status ?? "?");
            const target = deployment?.target ? ` · ${deployment.target}` : "";
            return (
              <Text key={`${name}-${index}`} color={TEXT_MUTED}>
                {`${name} · ${status}${target}`}
              </Text>
            );
          })}
          {deployments.length > 5 ? (
            <Text color={TEXT_DIM}>{`+${deployments.length - 5} more`}</Text>
          ) : null}
        </Box>
      );
    }

    if (parsed && typeof parsed === "object") {
      const deploymentId = String(parsed?.deployment_id ?? parsed?.id ?? "?");
      const status = parsed?.status ? `status: ${parsed.status}` : "";
      const name = parsed?.name ? `name: ${parsed.name}` : "";
      const target = parsed?.target ? `target: ${parsed.target}` : "";
      const image = parsed?.image ? `image: ${parsed.image}` : "";
      const health =
        typeof parsed?.healthy === "boolean"
          ? `healthy: ${parsed.healthy}`
          : typeof parsed?.health_ok === "boolean"
            ? `healthy: ${parsed.health_ok}`
            : "";
      const lines = [deploymentId, status, name, target, image, health].filter(Boolean);
      return (
        <Box marginTop={1} flexDirection="column">
          {lines.map((line, index) => (
            <Text key={`${deploymentId}-${index}`} color={TEXT_MUTED}>
              {line}
            </Text>
          ))}
        </Box>
      );
    }
  }

  if (root === "run" && parsed && typeof parsed === "object") {
    const jobId = String(parsed?.job_id ?? "?");
    const status =
      typeof parsed?.initial_status === "string"
        ? parsed.initial_status
        : parsed?.initial_status && typeof parsed.initial_status === "object"
          ? Object.keys(parsed.initial_status)[0] ?? "status"
          : parsed?.status_error
            ? `status unavailable`
            : "";
    const lines = [
      `job: ${jobId}`,
      parsed?.name ? `name: ${parsed.name}` : "",
      parsed?.backend ? `backend: ${parsed.backend}` : "",
      status ? `status: ${status}` : "",
      parsed?.image ? `image: ${parsed.image}` : "",
    ].filter(Boolean);
    return (
      <Box marginTop={1} flexDirection="column">
        {lines.map((line, index) => (
          <Text key={`${jobId}-${index}`} color={TEXT_MUTED}>
            {line}
          </Text>
        ))}
      </Box>
    );
  }

  if (root === "publish" && parsed && typeof parsed === "object") {
    const lines = [
      parsed?.target ? `target: ${parsed.target}` : "",
      parsed?.repo ? `repo: ${parsed.repo}` : "",
      parsed?.private !== undefined ? `private: ${parsed.private}` : "",
      parsed?.published_url ? `url: ${parsed.published_url}` : "",
      parsed?.path ? `path: ${parsed.path}` : "",
    ].filter(Boolean);
    if (lines.length) {
      return (
        <Box marginTop={1} flexDirection="column">
          {lines.map((line, index) => (
            <Text key={`${line}-${index}`} color={TEXT_MUTED}>
              {line}
            </Text>
          ))}
        </Box>
      );
    }
  }

  if (root === "ingest") {
    const items = Array.isArray(parsed) ? parsed : parsed ? [parsed] : [];
    if (!items.length) return null;
    return (
      <Box marginTop={1} flexDirection="column">
        <Text color={TEXT_DIM}>{`ingest items: ${items.length}`}</Text>
        {items.slice(0, 5).map((item: any, index: number) => {
          const path = String(item?.path ?? "?");
          const backend = String(item?.backend ?? "ingest");
          const detail =
            backend === "platform_text"
              ? `chunks=${item?.chunk_count ?? 0}`
              : item?.result?.row_count !== undefined
                ? `rows=${item.result.row_count} cols=${item.result.column_count ?? 0}`
                : previewValue(item);
          return (
            <Text key={`${path}-${index}`} color={TEXT_MUTED}>
              {`${path} · ${backend} · ${detail}`}
            </Text>
          );
        })}
      </Box>
    );
  }

  if (root === "research" && parsed && typeof parsed === "object") {
    const answer = parsed?.answer ? previewValue(parsed.answer, 140) : "";
    const sourceCount = Array.isArray(parsed?.sources) ? parsed.sources.length : 0;
    const eventCount = Array.isArray(parsed?.events) ? parsed.events.length : 0;
    const lines = [
      answer ? `answer: ${answer}` : "",
      sourceCount ? `sources: ${sourceCount}` : "",
      eventCount ? `events: ${eventCount}` : "",
    ].filter(Boolean);
    if (lines.length) {
      return (
        <Box marginTop={1} flexDirection="column">
          {lines.map((line, index) => (
            <Text key={`${line}-${index}`} color={TEXT_MUTED}>
              {line}
            </Text>
          ))}
        </Box>
      );
    }
  }

  if (root === "discourse" && parsed && typeof parsed === "object") {
    if (Array.isArray(parsed?.specs)) {
      const specs = parsed.specs;
      return (
        <Box marginTop={1} flexDirection="column">
          <Text color={TEXT_DIM}>{`specs: ${specs.length}`}</Text>
          {specs.slice(0, 6).map((spec: any, index: number) => (
            <Text key={`${spec?.id ?? index}`} color={TEXT_MUTED}>
              {`${spec?.slug ?? spec?.name ?? "spec"} · v${spec?.version ?? "?"}`}
            </Text>
          ))}
          {specs.length > 6 ? (
            <Text color={TEXT_DIM}>{`+${specs.length - 6} more`}</Text>
          ) : null}
        </Box>
      );
    }

    if (Array.isArray(parsed?.events)) {
      const events = parsed.events;
      const instanceId = parsed?.instance_id ? `instance: ${parsed.instance_id}` : "";
      return (
        <Box marginTop={1} flexDirection="column">
          {instanceId ? <Text color={TEXT_DIM}>{instanceId}</Text> : null}
          <Text color={TEXT_DIM}>{`events: ${events.length}`}</Text>
          {events.slice(0, 6).map((event: any, index: number) => {
            const step = String(event?.step ?? event?.event ?? "event");
            const detail =
              event?.agent_id
                ? `${event.agent_id}: ${previewValue(event?.content)}`
                : event?.round
                  ? `round ${event.round}`
                  : previewValue(event);
            return (
              <Text key={`${step}-${index}`} color={TEXT_MUTED}>
                {`${step} · ${detail}`}
              </Text>
            );
          })}
          {events.length > 6 ? (
            <Text color={TEXT_DIM}>{`+${events.length - 6} more`}</Text>
          ) : null}
        </Box>
      );
    }

    if (Array.isArray(parsed?.turns)) {
      const turns = parsed.turns;
      return (
        <Box marginTop={1} flexDirection="column">
          <Text color={TEXT_DIM}>{`turns: ${turns.length}`}</Text>
          {turns.slice(0, 6).map((turn: any, index: number) => (
            <Text key={`${turn?.id ?? index}`} color={TEXT_MUTED}>
              {`${turn?.agent_id ?? "agent"} · ${previewValue(turn?.content)}`}
            </Text>
          ))}
          {turns.length > 6 ? (
            <Text color={TEXT_DIM}>{`+${turns.length - 6} more`}</Text>
          ) : null}
        </Box>
      );
    }

    const lines = [
      parsed?.name ? `name: ${parsed.name}` : "",
      parsed?.slug ? `slug: ${parsed.slug}` : "",
      parsed?.status ? `status: ${parsed.status}` : "",
      parsed?.spec_id ? `spec: ${parsed.spec_id}` : "",
      parsed?.instance_id ? `instance: ${parsed.instance_id}` : "",
      parsed?.total_turns !== undefined ? `turns: ${parsed.total_turns}` : "",
      parsed?.total_llm_calls !== undefined ? `llm calls: ${parsed.total_llm_calls}` : "",
    ].filter(Boolean);

    if (lines.length) {
      return (
        <Box marginTop={1} flexDirection="column">
          {lines.map((line, index) => (
            <Text key={`${line}-${index}`} color={TEXT_MUTED}>
              {line}
            </Text>
          ))}
        </Box>
      );
    }
  }

  return null;
}

function formatElapsed(ms: number): string {
  if (ms >= 2000) return `${(ms / 1000).toFixed(1)}s`;
  if (ms > 0) return `${Math.round(ms)}ms`;
  return "";
}

function hasStructuredExecutionPayload(toolName: string, data: Record<string, any>): boolean {
  return (
    (toolName === "execute_bash" ||
      toolName === "execute_python" ||
      typeof data.invocation === "string") &&
    (
      typeof data.invocation === "string" ||
      typeof data.exit_code === "number" ||
      typeof data.stdout === "string" ||
      typeof data.stderr === "string" ||
      typeof data.error === "string"
    )
  );
}

// Tools that produce visible output get a bordered block.
const BLOCK_TOOLS = new Set([
  "execute", "bash", "shell", "write", "edit", "create",
  "python", "execute_python", "execute_bash", "run_code", "search_code",
  "read_file", "write_file", "edit_file",
  "list_bash_tasks", "read_bash_task", "stop_bash_task",
]);

/**
 * Inline tool — compact one-liner:  ✓ tool_name  120ms
 * Block tool — bordered container with output
 */
export function ToolCard({ cardType, toolName, elapsedMs, content, data, pending }: Props) {
  const isError = cardType === "error" || cardType === "error_partial";
  const isBlock = (BLOCK_TOOLS.has(toolName) || typeof data.invocation === "string") && content;
  const elapsed = formatElapsed(elapsedMs);
  const structuredExecution = hasStructuredExecutionPayload(toolName, data);

  if (pending) {
    // Still running — show spinner-style indicator
    return (
      <Box paddingLeft={3}>
        <Text color={TEXT_MUTED}>{"⠸ "}</Text>
        <Text color={TEXT_MUTED}>{toolName}</Text>
        {data?.summary ? (
          <Text color={TEXT_DIM}>{" "}{String(data.summary).slice(0, 60)}</Text>
        ) : null}
      </Box>
    );
  }

  if (isBlock || structuredExecution) {
    const preview = data?.preview ? String(data.preview) : "";
    const summary = data?.summary ? String(data.summary) : "";
    const stdout = typeof data?.stdout === "string" ? data.stdout.trim() : "";
    const stderr = typeof data?.stderr === "string" ? data.stderr.trim() : "";
    const errorText = !stderr && typeof data?.error === "string" ? data.error.trim() : "";
    const invocation = typeof data?.invocation === "string" ? data.invocation : "";
    const interpretation =
      typeof data?.return_code_interpretation === "string"
        ? data.return_code_interpretation
        : "";
    const exitCode = typeof data?.exit_code === "number" ? data.exit_code : undefined;
    const cwd = typeof data?.cwd === "string" ? data.cwd : "";
    const timedOut = !!data?.timed_out;
    const structuredCommandBody = renderStructuredCommandData(data);

    // Execution tools benefit from a structured block view: preview first, then
    // exit semantics and stdout/stderr as separate sections.
    return (
      <Pane
        color={isError ? ERROR : BORDER_AGENT}
        title={toolName}
        subtitle={
          elapsed
            ? isError
              ? `${elapsed} · failed`
              : elapsed
            : isError
              ? "failed"
              : undefined
        }
      >
        {preview ? (
          <Text color={TEXT_MUTED}>{preview}</Text>
        ) : null}

        {summary && summary !== preview ? (
          <Text color={TEXT_DIM}>{summary}</Text>
        ) : null}

        {invocation ? (
          <Text color={TEXT_MUTED}>{invocation}</Text>
        ) : null}

        {(exitCode !== undefined || interpretation || cwd || timedOut) ? (
          <Box flexDirection="column" marginTop={1}>
            {exitCode !== undefined ? (
              <Text color={TEXT_DIM}>{`exit code: ${exitCode}`}</Text>
            ) : null}
            {interpretation ? (
              <Text color={TEXT_DIM}>{interpretation}</Text>
            ) : null}
            {timedOut ? (
              <Text color={ERROR}>timed out</Text>
            ) : null}
            {cwd ? (
              <Text color={TEXT_DIM}>{`cwd: ${cwd}`}</Text>
            ) : null}
          </Box>
        ) : null}

        {structuredCommandBody}

        {stdout ? (
          <Box marginTop={1} flexDirection="column">
            <Text color={TEXT_DIM}>stdout</Text>
            <MarkdownText text={stdout} />
          </Box>
        ) : null}

        {stderr || errorText ? (
          <Box marginTop={1} flexDirection="column">
            <Text color={ERROR}>stderr</Text>
            <MarkdownText text={stderr || errorText} />
          </Box>
        ) : null}

        {!structuredExecution && content ? (
          <Box marginTop={0} flexDirection="column">
            <MarkdownText text={content} />
          </Box>
        ) : null}
      </Pane>
    );
  }

  // Inline tool — compact single line
  return (
    <Box paddingLeft={3}>
      <Text color={isError ? ERROR : SUCCESS}>{isError ? "✗" : "✓"}</Text>
      <Text color={TEXT}>{" "}{toolName}</Text>
      {elapsed ? <Text color={TEXT_DIM}>{" · "}{elapsed}</Text> : null}
      {content ? (
        <Text color={TEXT_MUTED}>{" "}{content.split("\n")[0]?.slice(0, 60)}</Text>
      ) : null}
    </Box>
  );
}

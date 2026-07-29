import { spawn } from "node:child_process";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";

type Wire = {
  session_id: string;
  transcript_path?: string;
  event: string;
  [key: string]: unknown;
};

type AssistantLike = {
  role?: unknown;
  content?: unknown;
  stopReason?: unknown;
  errorMessage?: unknown;
};

type Terminal = {
  stopReason?: string;
  summary?: string;
  error?: string;
};

function compact(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  return value.replace(/\s+/g, " ").trim().slice(0, 240) || undefined;
}

function bounded(value: unknown, max = 256): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  if (!trimmed) return undefined;
  return trimmed.length <= max ? trimmed : trimmed.slice(0, max);
}

function assistant(value: unknown): AssistantLike | undefined {
  if (typeof value !== "object" || value === null) return undefined;
  const message = value as AssistantLike;
  return message.role === "assistant" ? message : undefined;
}

function terminalFrom(value: unknown): Terminal | undefined {
  const message = assistant(value);
  if (!message) return undefined;

  let summary: string | undefined;
  if (typeof message.content === "string") {
    summary = compact(message.content);
  } else if (Array.isArray(message.content)) {
    for (const block of [...message.content].reverse()) {
      if (typeof block !== "object" || block === null) continue;
      const candidate = block as { type?: unknown; text?: unknown };
      if (candidate.type !== "text") continue;
      summary = compact(candidate.text);
      if (summary) break;
    }
  }

  return {
    stopReason:
      typeof message.stopReason === "string" ? message.stopReason : undefined,
    summary,
    error: compact(message.errorMessage),
  };
}

function toolActivity(tool: string, args: unknown): string {
  if (typeof args !== "object" || args === null) return tool;
  const input = args as Record<string, unknown>;
  const detail = [
    input.command,
    input.path,
    input.file_path,
    input.query,
    input.description,
  ].map(compact).find(Boolean);
  return compact(detail ? `${tool}: ${detail}` : tool) ?? tool;
}

function wire(ctx: ExtensionContext, event: string, fields: Record<string, unknown> = {}): Wire {
  return {
    session_id: ctx.sessionManager.getSessionId(),
    transcript_path: ctx.sessionManager.getSessionFile(),
    event,
    ...fields,
  };
}

function send(payload: Wire): Promise<void> {
  return new Promise((resolve) => {
    try {
      const child = spawn("gw", ["hook", "pi"], {
        stdio: ["pipe", "ignore", "ignore"],
      });
      let settled = false;
      const done = () => {
        if (settled) return;
        settled = true;
        resolve();
      };
      child.once("error", done);
      child.once("close", done);
      child.stdin.on("error", () => {});
      child.stdin.end(JSON.stringify(payload));
    } catch {
      resolve();
    }
  });
}

// Cooperative UI-prompt protocol on the shared `pi.events` bus. Any extension
// that opens a blocking UI prompt (`ctx.ui.confirm/select/input/editor`)
// participates by emitting the two topics documented in
// docs/provider-hooks.md; gw stays a passive observer and does not decide
// what counts as an Attention-worthy prompt. `pi.events.on(...)` returns an
// unsubscribe function, which is called on `session_shutdown` (including
// `reason: "reload"`) so that a rebound extension instance does not race
// against its predecessor's listeners.
const PROMPT_PROTOCOL_VERSION = 1;

type PromptOpened = {
  version?: unknown;
  id?: unknown;
  source?: unknown;
  summary?: unknown;
  kind?: unknown;
  tool?: unknown;
  toolCallId?: unknown;
};

type PromptClosed = {
  version?: unknown;
  id?: unknown;
  source?: unknown;
  outcome?: unknown;
};

type OpenPrompt = {
  attention_id: string;
  kind: "approval" | "question";
  summary: string;
  source: string;
  tool?: string;
  tool_call_id?: string;
};

function promptKind(value: unknown): "approval" | "question" | undefined {
  // Missing kind is accepted as approval, which is the safer default for a
  // blocking prompt. Any explicit non-supported kind is rejected so that
  // "notice" or a typo cannot masquerade as a real approval Attention.
  if (value === undefined || value === "approval") return "approval";
  if (value === "question") return "question";
  return undefined;
}

function promptKey(source: string, id: string): string {
  // Identity is scoped by source so two extensions can safely reuse the same
  // id string without shadowing each other's prompts.
  return `${source}\u0000${id}`;
}

export default function gw(pi: ExtensionAPI) {
  let sending = Promise.resolve();
  let terminal: Terminal | undefined;
  // (source, id) → open prompt metadata. A Map preserves insertion order so
  // we can pick the "most recently opened remaining prompt" when the
  // currently-shown one closes but others are still open.
  const openPrompts = new Map<string, OpenPrompt>();
  // The key whose Attention is currently the last event we published. When
  // it closes we must re-emit for another still-open prompt so the panel
  // does not regress to Working while a prompt is still waiting.
  let lastEmittedKey: string | undefined;
  // pi.events handlers do not receive ctx; cache the most recent one from
  // lifecycle callbacks so cross-extension prompt notifications can be
  // attributed to the current session.
  let latestCtx: ExtensionContext | undefined;
  // Retain unsubscribes so the rebound instance created by /reload, /new,
  // /resume, or /fork does not race the old listeners on the shared bus.
  const unsubscribes: Array<() => void> = [];

  const startupResumes = process.argv.some((arg) =>
    ["--session", "--session-id", "-c", "--continue", "-r", "--resume"].includes(arg),
  );

  const emit = (payload: Wire): Promise<void> => {
    sending = sending.then(() => send(payload), () => send(payload));
    return sending;
  };

  const tui = (ctx: ExtensionContext) => ctx.mode === "tui";

  const track = (ctx: ExtensionContext) => {
    if (tui(ctx)) latestCtx = ctx;
  };

  pi.on("session_start", async (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    terminal = undefined;
    openPrompts.clear();
    lastEmittedKey = undefined;
    const isNew =
      event.reason === "new" ||
      event.reason === "fork" ||
      (event.reason === "startup" && !startupResumes);
    await emit(
      wire(ctx, isNew ? "session_start" : "session_focus", {
        model: ctx.model?.id,
      }),
    );
  });

  pi.on("before_agent_start", async (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    terminal = undefined;
    await emit(wire(ctx, "turn_start", { summary: compact(event.prompt) }));
  });

  // A low-level run restarts during automatic retry or compaction recovery.
  // Drop the prior run's terminal message so an empty retry cannot inherit it.
  pi.on("agent_start", (_event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    terminal = undefined;
  });

  pi.on("tool_execution_start", async (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    await emit(
      wire(ctx, "tool_start", {
        activity: toolActivity(event.toolName, event.args),
      }),
    );
  });

  pi.on("turn_end", (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    terminal = terminalFrom(event.message) ?? terminal;
  });

  pi.on("agent_end", (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    for (const message of [...event.messages].reverse()) {
      const next = terminalFrom(message);
      if (next) {
        terminal = next;
        break;
      }
    }
  });

  pi.on("agent_settled", async (_event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    if (terminal?.stopReason === "error") {
      await emit(
        wire(ctx, "agent_settled", {
          status: "error",
          reason: terminal.stopReason,
          summary: terminal.error ?? terminal.summary,
        }),
      );
    } else {
      await emit(
        wire(ctx, "agent_settled", {
          status: "done",
          summary: terminal?.summary,
        }),
      );
    }
  });

  pi.on("session_shutdown", async (event, ctx) => {
    if (!tui(ctx)) return;
    track(ctx);
    for (const off of unsubscribes) {
      try {
        off();
      } catch {}
    }
    unsubscribes.length = 0;
    openPrompts.clear();
    lastEmittedKey = undefined;
    if (event.reason === "reload") {
      // The same Session continues in the rebound extension instance.
      // Do not emit `session_end` — gw would treat it as a real session end.
      return;
    }
    await emit(wire(ctx, "session_end"));
  });

  // Save unsubscribes so the rebound instance created by /reload, /new,
  // /resume, or /fork does not race the old listeners on the shared bus.

  const onOpened = (data: unknown): void => {
    const currentCtx = latestCtx;
    if (!currentCtx || !tui(currentCtx)) return;
    const payload =
      typeof data === "object" && data !== null ? (data as PromptOpened) : ({} as PromptOpened);
    if (payload.version !== PROMPT_PROTOCOL_VERSION) return;
    const source = bounded(payload.source);
    const id = bounded(payload.id);
    const summary = compact(payload.summary);
    const kind = promptKind(payload.kind);
    if (!source || !id || !summary || !kind) return;

    const key = promptKey(source, id);
    // Duplicate opens are treated as idempotent updates: replace the entry,
    // move to the end of the insertion order, and re-emit so the summary
    // reflects the latest text.
    openPrompts.delete(key);
    const info: OpenPrompt = {
      attention_id: `${source}:${id}`,
      kind,
      summary,
      source,
      tool: bounded(payload.tool),
      tool_call_id: bounded(payload.toolCallId),
    };
    openPrompts.set(key, info);
    lastEmittedKey = key;
    void emit(
      wire(currentCtx, "attention", {
        attention_id: info.attention_id,
        kind: info.kind,
        summary: info.summary,
        source: info.source,
        tool: info.tool,
        tool_call_id: info.tool_call_id,
      }),
    );
  };

  const onClosed = (data: unknown): void => {
    const currentCtx = latestCtx;
    if (!currentCtx || !tui(currentCtx)) return;
    const payload =
      typeof data === "object" && data !== null ? (data as PromptClosed) : ({} as PromptClosed);
    if (payload.version !== PROMPT_PROTOCOL_VERSION) return;
    const source = bounded(payload.source);
    const id = bounded(payload.id);
    if (!source || !id) return;

    const key = promptKey(source, id);
    const entry = openPrompts.get(key);
    if (!entry) return;
    openPrompts.delete(key);
    const outcome = bounded(payload.outcome);

    if (openPrompts.size === 0) {
      lastEmittedKey = undefined;
      // Attention is single-shot on gw's side; a Heartbeat is the neutral
      // "agent is running again" signal. If Pi is actually idle when this
      // fires, gw will decay through Working → Stale until the next real
      // event — producers should only emit these topics for prompts that
      // suspend an active turn (see docs/provider-hooks.md).
      void emit(
        wire(currentCtx, "attention_end", {
          attention_id: entry.attention_id,
          outcome,
        }),
      );
      return;
    }

    if (key !== lastEmittedKey) return;
    // The still-shown prompt just closed but others remain open. Re-emit
    // for the most recently opened remaining prompt so Attention stays.
    const remaining = Array.from(openPrompts.entries());
    const [nextKey, next] = remaining[remaining.length - 1];
    lastEmittedKey = nextKey;
    void emit(
      wire(currentCtx, "attention", {
        attention_id: next.attention_id,
        kind: next.kind,
        summary: next.summary,
        source: next.source,
        tool: next.tool,
        tool_call_id: next.tool_call_id,
      }),
    );
  };

  unsubscribes.push(pi.events.on("ui:prompt:opened", onOpened) as () => void);
  unsubscribes.push(pi.events.on("ui:prompt:closed", onClosed) as () => void);
}

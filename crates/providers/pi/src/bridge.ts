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

export default function gw(pi: ExtensionAPI) {
  let sending = Promise.resolve();
  let terminal: Terminal | undefined;

  const startupResumes = process.argv.some((arg) =>
    ["--session", "--session-id", "-c", "--continue", "-r", "--resume"].includes(arg),
  );

  const emit = (payload: Wire): Promise<void> => {
    sending = sending.then(() => send(payload), () => send(payload));
    return sending;
  };

  const tui = (ctx: ExtensionContext) => ctx.mode === "tui";

  pi.on("session_start", async (event, ctx) => {
    if (!tui(ctx)) return;
    terminal = undefined;
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
    terminal = undefined;
    await emit(wire(ctx, "turn_start", { summary: compact(event.prompt) }));
  });

  // A low-level run restarts during automatic retry or compaction recovery.
  // Drop the prior run's terminal message so an empty retry cannot inherit it.
  pi.on("agent_start", (_event, ctx) => {
    if (!tui(ctx)) return;
    terminal = undefined;
  });

  pi.on("tool_execution_start", async (event, ctx) => {
    if (!tui(ctx)) return;
    await emit(
      wire(ctx, "tool_start", {
        activity: toolActivity(event.toolName, event.args),
      }),
    );
  });

  pi.on("turn_end", (event, ctx) => {
    if (!tui(ctx)) return;
    terminal = terminalFrom(event.message) ?? terminal;
  });

  pi.on("agent_end", (event, ctx) => {
    if (!tui(ctx)) return;
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
    if (!tui(ctx) || event.reason === "reload") return;
    await emit(wire(ctx, "session_end"));
  });
}

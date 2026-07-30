import type { Plugin } from "@opencode-ai/plugin";

type Wire = Record<string, unknown> & { session_id: string; event: string };
type SessionInfo = {
  id?: unknown;
  parentID?: unknown;
};

function compact(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  return value.replace(/\s+/g, " ").trim().slice(0, 240) || undefined;
}

function detail(value: unknown): string | undefined {
  if (!value || typeof value !== "object") return undefined;
  const input = value as Record<string, unknown>;
  return compact(
    input.command ?? input.filePath ?? input.file_path ?? input.path ?? input.query ?? input.description,
  );
}

function errorText(value: unknown): string | undefined {
  if (typeof value === "string") return compact(value);
  if (!value || typeof value !== "object") return undefined;
  const error = value as Record<string, unknown>;
  const data = error.data as Record<string, unknown> | undefined;
  return compact(data?.message ?? error.message ?? error.name ?? error.type);
}

export const GwPlugin: Plugin = async () => {
  const rootSessions = new Set<string>();
  const childSessions = new Set<string>();
  const started = new Set<string>();
  const summaries = new Map<string, string>();
  let sending = Promise.resolve();

  const classify = (info: SessionInfo): string | undefined => {
    if (typeof info.id !== "string") return undefined;
    if (typeof info.parentID === "string") {
      childSessions.add(info.id);
      rootSessions.delete(info.id);
      return undefined;
    }
    rootSessions.add(info.id);
    childSessions.delete(info.id);
    return info.id;
  };

  const root = (sessionID: unknown) =>
    typeof sessionID === "string" && !childSessions.has(sessionID) ? sessionID : undefined;

  const send = (payload: Wire): Promise<void> => {
    sending = sending.then(async () => {
      try {
        const proc = Bun.spawn(["gw", "hook", "opencode"], {
          stdin: "pipe",
          stdout: "ignore",
          stderr: "ignore",
        });
        proc.stdin.write(JSON.stringify(payload));
        proc.stdin.end();
        await proc.exited;
      } catch {}
    });
    return sending;
  };

  const emit = (sessionID: string, event: string, fields: Record<string, unknown> = {}) =>
    send({ session_id: sessionID, event, ...fields });

  return {
    event: async ({ event }) => {
      const eventType: string = event.type;
      const properties = event.properties as Record<string, unknown>;

      if (eventType === "session.created") {
        const info = properties.info as SessionInfo;
        const sessionID = classify(info);
        if (!sessionID || started.has(sessionID)) return;
        started.add(sessionID);
        await emit(sessionID, "session_start");
        return;
      }

      if (eventType === "session.updated") {
        classify(properties.info as SessionInfo);
        return;
      }

      if (eventType === "session.deleted") {
        const info = properties.info as SessionInfo;
        const sessionID = root(info.id);
        if (!sessionID) return;
        await emit(sessionID, "session_end");
        rootSessions.delete(sessionID);
        started.delete(sessionID);
        summaries.delete(sessionID);
        return;
      }

      const sessionID = root(properties.sessionID);
      if (!sessionID) return;

      if (eventType === "session.status") {
        const status = properties.status as { type?: unknown } | undefined;
        if (status?.type === "busy") {
          await emit(sessionID, "session_focus");
        } else if (status?.type === "retry") {
          await emit(sessionID, "tool_start", {
            activity: compact((status as { message?: unknown }).message) ?? "retrying",
          });
        } else if (status?.type === "idle") {
          await emit(sessionID, "turn_end", { summary: summaries.get(sessionID) });
        }
        return;
      }

      if (eventType === "message.part.updated") {
        const part = properties.part as Record<string, unknown> | undefined;
        if (part?.type === "text") {
          const summary = compact(part.text);
          if (summary) summaries.set(sessionID, summary);
        }
        return;
      }

      if (eventType === "permission.asked" || eventType === "permission.updated") {
        const permission = compact(properties.permission ?? properties.type ?? properties.title) ?? "permission";
        const rawPatterns = properties.patterns ?? properties.pattern;
        const patterns = Array.isArray(rawPatterns)
          ? rawPatterns.map(compact).filter(Boolean).join(", ")
          : compact(rawPatterns);
        await emit(sessionID, "permission_asked", {
          summary: compact(patterns ? `${permission}: ${patterns}` : permission),
        });
        return;
      }

      if (eventType === "permission.replied") {
        await emit(sessionID, "permission_replied", {
          activity: compact(properties.reply ?? properties.response),
        });
        return;
      }

      if (eventType === "session.error") {
        const error = properties.error as Record<string, unknown> | undefined;
        await emit(sessionID, "turn_error", {
          reason: compact(error?.name ?? error?.type) ?? "error",
          summary: errorText(error),
        });
        return;
      }

    },

    "chat.message": async (input, output) => {
      const sessionID = root(input.sessionID);
      if (!sessionID) return;
      const text = output.parts.find((part) => part.type === "text");
      const summary = text && "text" in text ? compact(text.text) : undefined;
      const model = input.model ? `${input.model.providerID}/${input.model.modelID}` : undefined;
      if (!started.has(sessionID)) {
        started.add(sessionID);
        await emit(sessionID, "session_start", { model });
      }
      await emit(sessionID, "turn_start", { summary });
    },

    "tool.execute.before": async (input, output) => {
      const sessionID = root(input.sessionID);
      if (!sessionID) return;
      const toolDetail = detail(output.args);
      await emit(sessionID, "tool_start", {
        activity: compact(toolDetail ? `${input.tool}: ${toolDetail}` : input.tool),
      });
    },
  };
};

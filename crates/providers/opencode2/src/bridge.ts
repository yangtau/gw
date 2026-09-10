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
    input.command ??
      input.filePath ??
      input.file_path ??
      input.path ??
      input.query ??
      input.description ??
      input.text,
  );
}

function errorText(value: unknown): string | undefined {
  if (typeof value === "string") return compact(value);
  if (!value || typeof value !== "object") return undefined;
  const error = value as Record<string, unknown>;
  const data = error.data as Record<string, unknown> | undefined;
  return compact(data?.message ?? error.message ?? error.name ?? error.type);
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value && typeof value === "object" && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function eventType(event: Record<string, unknown>): string {
  const raw = typeof event.type === "string" ? event.type : "";
  return raw.replace(/\.\d+$/, "");
}

function payload(event: Record<string, unknown>): Record<string, unknown> {
  return record(event.data) ?? record(event.properties) ?? event;
}

function infoOf(body: Record<string, unknown>): SessionInfo {
  return record(body.info) ?? {};
}

function sessionIDOf(body: Record<string, unknown>, info: SessionInfo = infoOf(body)): string | undefined {
  if (typeof body.sessionID === "string") return body.sessionID;
  if (typeof info.id === "string") return info.id;
  return undefined;
}

function promptText(value: unknown): string | undefined {
  if (typeof value === "string") return compact(value);
  const prompt = record(value);
  if (!prompt) return undefined;
  return compact(prompt.text) ?? compact(prompt.prompt);
}

export default {
  id: "gw.opencode2",
  async setup(ctx: {
    event: { subscribe: (options?: { signal?: AbortSignal }) => AsyncIterable<unknown> };
    session: { hook: (name: string, callback: (event: Record<string, unknown>) => unknown) => Promise<unknown> };
    tool: { hook: (name: string, callback: (event: Record<string, unknown>) => unknown) => Promise<unknown> };
    permission: { hook: (name: string, callback: (event: Record<string, unknown>) => unknown) => Promise<unknown> };
  }) {
    const childSessions = new Set<string>();
    const started = new Set<string>();
    const summaries = new Map<string, string>();
    let sending = Promise.resolve();

    const classify = (info: SessionInfo): string | undefined => {
      if (typeof info.id !== "string") return undefined;
      if (typeof info.parentID === "string") {
        childSessions.add(info.id);
        return undefined;
      }
      childSessions.delete(info.id);
      return info.id;
    };

    const root = (sessionID: unknown) =>
      typeof sessionID === "string" && !childSessions.has(sessionID) ? sessionID : undefined;

    const send = (wire: Wire): Promise<void> => {
      sending = sending.then(async () => {
        try {
          const proc = Bun.spawn(["gw", "hook", "opencode2"], {
            stdin: "pipe",
            stdout: "ignore",
            stderr: "ignore",
          });
          proc.stdin.write(JSON.stringify(wire));
          proc.stdin.end();
          await proc.exited;
        } catch {}
      });
      return sending;
    };

    const emit = (sessionID: string, event: string, fields: Record<string, unknown> = {}) =>
      send({ session_id: sessionID, event, ...fields });

    const remember = (sessionID: string, model?: string) => {
      if (started.has(sessionID)) return Promise.resolve();
      started.add(sessionID);
      return emit(sessionID, "session_start", model ? { model } : {});
    };

    const forget = (sessionID: string) => {
      started.delete(sessionID);
      summaries.delete(sessionID);
    };

    const modelOf = (value: unknown): string | undefined => {
      const model = record(value);
      if (!model) return undefined;
      const providerID = compact(model.providerID);
      const id = compact(model.id ?? model.modelID);
      if (providerID && id) return `${providerID}/${id}`;
      return id ?? providerID;
    };

    const controller = new AbortController();
    void (async () => {
      try {
        for await (const raw of ctx.event.subscribe({ signal: controller.signal })) {
          const event = record(raw);
          if (!event) continue;
          const type = eventType(event);
          const body = payload(event);
          const info = infoOf(body);

          if (type === "session.created") {
            const sessionID = classify(info) ?? classify({ id: body.sessionID, parentID: info.parentID });
            if (!sessionID) continue;
            void remember(sessionID, modelOf(body.model ?? info));
            continue;
          }

          if (type === "session.updated") {
            classify(info);
            continue;
          }

          if (type === "session.deleted") {
            const sessionID = root(sessionIDOf(body, info));
            if (!sessionID) continue;
            void emit(sessionID, "session_end");
            forget(sessionID);
            continue;
          }

          const sessionID = root(sessionIDOf(body, info));
          if (!sessionID) continue;

          if (type === "session.status") {
            const status = record(body.status);
            const statusType = compact(status?.type ?? body.type);
            if (statusType === "busy") {
              void emit(sessionID, "session_focus");
            } else if (statusType === "retry") {
              void emit(sessionID, "tool_start", {
                activity: compact(status?.message) ?? "retrying",
              });
            } else if (statusType === "idle") {
              void emit(sessionID, "turn_end", { summary: summaries.get(sessionID) });
            }
            continue;
          }

          if (type === "session.idle" || type === "session.execution.succeeded") {
            void emit(sessionID, "turn_end", { summary: summaries.get(sessionID) });
            continue;
          }

          if (
            type === "session.next.prompted" ||
            type === "session.next.prompt.admitted" ||
            type === "session.inbox.enqueued"
          ) {
            const item = record(body.item);
            const summary =
              promptText(body.prompt) ??
              promptText(item) ??
              compact(body.text) ??
              compact(item?.text);
            const model = modelOf(body.model);
            void remember(sessionID, model);
            if (summary) void emit(sessionID, "turn_start", { summary });
            continue;
          }

          if (
            type === "session.execution.started" ||
            type === "session.step.started" ||
            type === "session.next.step.started"
          ) {
            void remember(sessionID, modelOf(body.model));
            void emit(sessionID, "session_focus");
            continue;
          }

          if (
            type === "session.execution.failed" ||
            type === "session.step.failed" ||
            type === "session.next.step.failed" ||
            type === "session.error"
          ) {
            const error = record(body.error) ?? body;
            void emit(sessionID, "turn_error", {
              reason: compact(error.name ?? error.type) ?? "error",
              summary: errorText(error),
            });
            continue;
          }

          if (
            type === "session.tool.started" ||
            type === "session.tool.called" ||
            type === "session.next.tool.called"
          ) {
            const tool = compact(body.tool) ?? "tool";
            const toolDetail = detail(body.input);
            void emit(sessionID, "tool_start", {
              activity: compact(toolDetail ? `${tool}: ${toolDetail}` : tool),
            });
            continue;
          }

          if (type === "session.next.retried" || type === "session.step.retried") {
            void emit(sessionID, "tool_start", {
              activity: errorText(body.error) ?? "retrying",
            });
            continue;
          }

          if (
            type === "session.text.ended" ||
            type === "session.next.text.ended" ||
            type === "message.part.updated"
          ) {
            const part = record(body.part);
            const text = compact(body.text) ?? (part?.type === "text" ? compact(part.text) : undefined);
            if (text) summaries.set(sessionID, text);
            continue;
          }

          if (
            type === "permission.asked" ||
            type === "permission.updated" ||
            type === "permission.v2.asked"
          ) {
            const permission =
              compact(body.action ?? body.permission ?? body.type ?? body.title) ?? "permission";
            const rawPatterns = body.resources ?? body.patterns ?? body.pattern;
            const patterns = Array.isArray(rawPatterns)
              ? rawPatterns.map(compact).filter(Boolean).join(", ")
              : compact(rawPatterns);
            await emit(sessionID, "permission_asked", {
              summary: compact(patterns ? `${permission}: ${patterns}` : permission),
            });
            continue;
          }

          if (type === "permission.replied" || type === "permission.v2.replied") {
            const reply = record(body.reply) ?? body;
            await emit(sessionID, "permission_replied", {
              activity: compact(reply.reply ?? reply.response ?? body.reply ?? body.response),
            });
            continue;
          }

          if (type === "question.asked" || type === "question.v2.asked") {
            void emit(sessionID, "question_asked", {
              summary: compact(body.question ?? body.title ?? body.text) ?? "question",
            });
          }
        }
      } catch {}
    })();

    const onPrompt = async (event: Record<string, unknown>) => {
      const sessionID = root(sessionIDOf(event));
      if (!sessionID) return;
      const summary = promptText(event.prompt) ?? compact(event.text);
      const model = modelOf(event.model);
      await remember(sessionID, model);
      await emit(sessionID, "turn_start", { summary });
    };

    const onTool = async (event: Record<string, unknown>) => {
      const sessionID = root(sessionIDOf(event));
      if (!sessionID) return;
      const tool = compact(event.tool) ?? "tool";
      const toolDetail = detail(event.input ?? event.args);
      await emit(sessionID, "tool_start", {
        activity: compact(toolDetail ? `${tool}: ${toolDetail}` : tool),
      });
    };

    const onPermission = async (event: Record<string, unknown>) => {
      if (event.effect !== "ask") return;
      const sessionID = root(sessionIDOf(event));
      if (!sessionID) return;
      const permission = compact(event.action ?? event.permission) ?? "permission";
      const rawPatterns = event.resources ?? event.patterns;
      const patterns = Array.isArray(rawPatterns)
        ? rawPatterns.map(compact).filter(Boolean).join(", ")
        : compact(rawPatterns);
      await emit(sessionID, "permission_asked", {
        summary: compact(patterns ? `${permission}: ${patterns}` : permission),
      });
    };

    await Promise.allSettled([
      ctx.session.hook("prompt", onPrompt),
      ctx.tool.hook("execute.before", onTool),
      ctx.permission.hook("evaluate", onPermission),
    ]);

    return async () => {
      controller.abort();
      await sending;
    };
  },
};

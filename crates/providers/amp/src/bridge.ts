import type {
  PluginAPI,
  PluginThread,
  ThreadID,
  ThreadMessage,
  ThreadState,
} from "@ampcode/plugin";

type Wire = Record<string, unknown> & { thread_id: ThreadID; event: string };
type ThreadCache = {
  prompt?: string;
  tool?: string;
  terminal?: Wire;
  state?: ThreadState;
};

function compact(value: unknown): string | undefined {
  if (Array.isArray(value)) value = value.join(" ");
  if (typeof value !== "string") return undefined;
  return value.replace(/\s+/g, " ").trim().slice(0, 240) || undefined;
}

function toolSummary(input: Record<string, unknown>): string | undefined {
  return compact(
    input.command ??
      input.file_path ??
      input.path ??
      input.query ??
      input.description,
  );
}

function assistantSummary(messages: ThreadMessage[]): string | undefined {
  for (const message of [...messages].reverse()) {
    if (message.role !== "assistant") continue;
    for (const block of [...message.content].reverse()) {
      if (block.type !== "text") continue;
      const text = compact(block.text);
      if (text) return text;
    }
  }
  return undefined;
}

export default function gw(amp: PluginAPI) {
  if (
    process.argv.some((arg) =>
      ["--no-tui", "-x", "--execute"].includes(arg),
    )
  ) {
    return;
  }

  const threads = new Map<ThreadID, ThreadCache>();
  const watched = new Set<ThreadID>();
  let sending = Promise.resolve();
  let foreground = amp.activeThread.current?.id;
  let focused: ThreadID | undefined;
  let focusEpoch = 0;

  const current = (id: ThreadID) => foreground === id;
  const cacheFor = (id: ThreadID) => {
    let cache = threads.get(id);
    if (!cache) {
      cache = {};
      threads.set(id, cache);
    }
    return cache;
  };

  function emit(payload: Wire, epoch?: number): Promise<boolean> {
    let sent = false;
    sending = sending.then(async () => {
      // Re-check at execution time: a queued event from the old foreground
      // thread must not take ownership back after a fast thread switch.
      if (!current(payload.thread_id) || (epoch !== undefined && epoch !== focusEpoch)) {
        return;
      }
      try {
        const proc = Bun.spawn(["gw", "hook", "amp"], {
          stdin: "pipe",
          stdout: "ignore",
          stderr: "ignore",
        });
        proc.stdin.write(JSON.stringify(payload));
        proc.stdin.end();
        sent = (await proc.exited) === 0;
        if (!sent) amp.logger.log("gw hook amp exited unsuccessfully");
      } catch (error) {
        amp.logger.log(`gw hook amp failed: ${String(error)}`);
      }
    });
    return sending.then(() => sent);
  }

  function watch(thread: PluginThread) {
    const id = thread.id;
    const cache = cacheFor(id);
    if (watched.has(id)) return;
    watched.add(id);
    thread.state.subscribe((state) => {
      const previous = cache.state;
      cache.state = state;
      // The focus path replays the initial state. Only transitions observed
      // after subscription belong here.
      if (previous === undefined || !current(id)) return;
      if (state === "awaiting-approval" && previous !== state) {
        void emit({
          thread_id: id,
          event: "approval",
        });
      } else if (state === "running" && previous === "awaiting-approval") {
        void emit({ thread_id: id, event: "tool_result", tool: cache.tool });
      }
    });
  }

  async function focus(id: ThreadID) {
    if (focused === id && current(id)) return;
    focused = id;
    const epoch = ++focusEpoch;
    const thread = amp.threads.get(id);
    const cache = cacheFor(id);
    void emit({ thread_id: id, event: "session_focus" }, epoch);
    watch(thread);

    if (cache.state === undefined) {
      try {
        const state = await thread.state.get();
        // A subscription update received while get() was in flight is newer.
        if (cache.state === undefined) cache.state = state;
      } catch (error) {
        amp.logger.log(`gw could not read Amp thread state: ${String(error)}`);
      }
    }
    if (!current(id) || epoch !== focusEpoch) return;

    if (cache.terminal) {
      const terminal = cache.terminal;
      if (await emit(terminal, epoch)) {
        if (cache.terminal === terminal) cache.terminal = undefined;
      }
    } else if (cache.state === "running") {
      void emit({
        thread_id: id,
        event: "agent_start",
        message: cache.prompt,
      }, epoch);
      if (cache.tool) {
        void emit({ thread_id: id, event: "tool_result", tool: cache.tool }, epoch);
      }
    } else if (cache.state === "awaiting-approval") {
      void emit({
        thread_id: id,
        event: "approval",
      }, epoch);
    } else if (cache.state === "error") {
      void emit({ thread_id: id, event: "state_error" }, epoch);
    }
  }

  amp.activeThread.subscribe((thread) => {
    foreground = thread?.id;
    if (!thread) {
      focused = undefined;
      focusEpoch += 1;
      return;
    }
    void focus(thread.id);
  });

  amp.on("session.start", (event, ctx) => {
    watch(ctx.thread);
    if (current(event.thread.id)) void focus(event.thread.id);
  });

  amp.on("agent.start", async (event, ctx) => {
    watch(ctx.thread);
    const cache = cacheFor(event.thread.id);
    cache.prompt = compact(event.message);
    cache.terminal = undefined;
    if (current(event.thread.id)) {
      await emit({
        thread_id: event.thread.id,
        event: "agent_start",
        message: cache.prompt,
      });
    }
    return {};
  });

  amp.on("tool.result", async (event, ctx) => {
    watch(ctx.thread);
    const cache = cacheFor(event.thread.id);
    cache.tool = [compact(event.tool), toolSummary(event.input)]
      .filter(Boolean)
      .join(": ");
    if (current(event.thread.id)) {
      await emit({
        thread_id: event.thread.id,
        event: "tool_result",
        tool: cache.tool,
      });
    }
  });

  amp.on("agent.end", async (event, ctx) => {
    watch(ctx.thread);
    const cache = cacheFor(event.thread.id);
    const payload: Wire = {
      thread_id: event.thread.id,
      event: "agent_end",
      status: event.status,
      summary: assistantSummary(event.messages),
    };
    if (!current(event.thread.id) || !(await emit(payload))) {
      cache.terminal = payload;
    }
  });

  if (foreground) void focus(foreground);
}

// The staff chat: a sidebar of conversations (a drawer on phones) and the open conversation.
import { useCallback, useEffect, useMemo, useRef, useState } from "preact/hooks";
import { showModelPicker, usdAmount } from "./rules.ts";
import { api, ApiError, type ChatEvent, type Conversation, type ConversationSummary, type Me, type Message, type PresentedFile, type Turn } from "./api.ts";
import { locale, normaliseLocale, relativeTime, saveLocale, savedLocale, setLocale, t, type Locale } from "./i18n.ts";
import { greeting } from "./greeting.ts";
import { Icon } from "./icons.tsx";
import { renderMarkdown } from "./md.ts";
import { describeTool, toolIcon } from "./tools.ts";
import { isActive, mergeEvent, replyText, viewTurn, type Block, type TurnView } from "./turns.ts";
import { ShareSheet, SharedView, shareRoute } from "./share.tsx";

const coarse = () => typeof matchMedia === "function" && matchMedia("(pointer: coarse)").matches;

function routeId(): string | null {
  const m = /^\/c\/([0-9a-fA-F-]{8,})\/?$/.exec(location.pathname);
  return m ? m[1]! : null;
}

// ---- app shell ------------------------------------------------------------------------------------

export function App() {
  const [me, setMe] = useState<Me | null>(null);
  const [blocked, setBlocked] = useState<"denied" | "failed" | null>(null);
  const [, setLoc] = useState<Locale>(locale());
  const [list, setList] = useState<ConversationSummary[]>([]);
  const [current, setCurrent] = useState<string | null>(routeId());
  // a shared conversation (/s/<id>): read-only, with "continue conversation"
  const [shareId, setShareId] = useState<string | null>(shareRoute());
  const [shareTitle, setShareTitle] = useState("");
  const [sharing, setSharing] = useState(false);
  // The open chat view; a chat created by its own first keystroke keeps its view (and the text being typed).
  const [viewKey, setViewKey] = useState(0);
  const [drawer, setDrawer] = useState(false);

  useEffect(() => {
    api
      .me()
      .then((m) => {
        setLocale(savedLocale() ?? normaliseLocale(m.product.locale));
        setLoc(locale());
        document.title = m.product.name;
        setMe(m);
      })
      .catch((e: ApiError) => {
        setLocale(savedLocale() ?? normaliseLocale(navigator.language));
        setBlocked(e.status === 401 || e.status === 403 ? "denied" : "failed");
      });
  }, []);

  const refreshList = useCallback(() => {
    api.conversations().then(setList).catch(() => {});
  }, []);
  useEffect(() => {
    if (!me) return;
    refreshList();
    const iv = setInterval(refreshList, 30_000);
    const vis = () => document.visibilityState === "visible" && refreshList();
    document.addEventListener("visibilitychange", vis);
    return () => {
      clearInterval(iv);
      document.removeEventListener("visibilitychange", vis);
    };
  }, [me, refreshList]);

  useEffect(() => {
    const pop = () => {
      setCurrent(routeId());
      setShareId(shareRoute());
      setViewKey((k) => k + 1);
    };
    addEventListener("popstate", pop);
    return () => removeEventListener("popstate", pop);
  }, []);

  const open = (id: string | null) => {
    history.pushState(null, "", id ? `/c/${id}` : "/");
    setCurrent(id);
    setShareId(null);
    setViewKey((k) => k + 1);
    setDrawer(false);
  };

  const changeLocale = (l: Locale) => {
    setLocale(l);
    saveLocale(l);
    setLoc(l);
  };

  if (blocked) return <Blocked kind={blocked} />;
  if (!me) return <div class="boot" aria-busy="true" />;

  const currentTitle = shareId ? shareTitle : list.find((c) => c.id === current)?.title || (current ? "" : t().newChat);

  return (
    <div class={`shell ${drawer ? "drawer-open" : ""}`}>
      <Sidebar
        me={me}
        list={list}
        current={current}
        onOpen={open}
        onRenamed={(id, title) => setList((l) => l.map((c) => (c.id === id ? { ...c, title } : c)))}
        onDeleted={(id) => {
          setList((l) => l.filter((c) => c.id !== id));
          if (id === current) open(null);
        }}
        onClose={() => setDrawer(false)}
        onLocale={changeLocale}
      />
      <div class="scrim" onClick={() => setDrawer(false)} aria-hidden="true" />
      <main class="main">
        <header class="topbar">
          <button class="icon-btn only-narrow" aria-label={t().menu} onClick={() => setDrawer(true)}>
            <Icon name="menu" />
          </button>
          <h1 class="topbar-title">{currentTitle || t().untitled}</h1>
          {current && !shareId && (
            <button class="icon-btn" aria-label={t().share} title={t().share} onClick={() => setSharing(true)}>
              <Icon name="share" />
            </button>
          )}
          <button class="icon-btn only-narrow" aria-label={t().newChat} onClick={() => open(null)}>
            <Icon name="plus" />
          </button>
        </header>
        {shareId ? (
          <SharedView
            key={`s${viewKey}`}
            id={shareId}
            onTitle={setShareTitle}
            onContinued={(id) => {
              refreshList();
              open(id);
            }}
          />
        ) : (
          <ChatView
            key={viewKey}
            me={me}
            id={current}
            onCreated={(id) => {
              history.replaceState(null, "", `/c/${id}`);
              setCurrent(id);
              refreshList();
            }}
            onActivity={refreshList}
            onTitle={(id, title) => setList((l) => l.map((c) => (c.id === id ? { ...c, title } : c)))}
          />
        )}
      </main>
      {sharing && current && <ShareSheet me={me} convId={current} onClose={() => setSharing(false)} />}
    </div>
  );
}

function Blocked({ kind }: { kind: "denied" | "failed" }) {
  const s = t();
  return (
    <div class="blocked">
      <div class="blocked-card">
        <Icon name={kind === "denied" ? "lock" : "alert"} size={28} />
        <h1>{kind === "denied" ? s.notAllowedTitle : s.error}</h1>
        <p>{kind === "denied" ? s.notAllowedBody : s.loadFailed}</p>
      </div>
    </div>
  );
}

export function Logo({ me }: { me: Me }) {
  const [broken, setBroken] = useState(false);
  const src = me.product.logoUrl || me.product.logo;
  if (src && !broken) return <img class="logo" src={src} alt="" onError={() => setBroken(true)} />;
  // "YYC AI" → "YYC": a short all-capitals first word is the mark; otherwise the words' initials.
  const words = me.product.name.trim().split(/\s+/);
  const initials = /^[A-Z0-9]{2,4}$/.test(words[0] ?? "") ? words[0]! : words.map((w) => w[0]).join("").slice(0, 3);
  return (
    <span class="logo logo-mark" aria-hidden="true">
      {initials}
    </span>
  );
}

// ---- sidebar --------------------------------------------------------------------------------------

function Sidebar(p: {
  me: Me;
  list: ConversationSummary[];
  current: string | null;
  onOpen: (id: string | null) => void;
  onRenamed: (id: string, title: string) => void;
  onDeleted: (id: string) => void;
  onClose: () => void;
  onLocale: (l: Locale) => void;
}) {
  const s = t();
  return (
    <nav class="sidebar" aria-label={s.chats}>
      <div class="sidebar-head">
        <Logo me={p.me} />
        <span class="product">{p.me.product.name}</span>
        <button class="icon-btn only-narrow" aria-label={s.closeMenu} onClick={p.onClose}>
          <Icon name="close" />
        </button>
      </div>
      <button class="new-chat" onClick={() => p.onOpen(null)}>
        <Icon name="plus" size={18} />
        <span>{s.newChat}</span>
      </button>
      <div class="list" role="list">
        {p.list.length === 0 && <p class="list-empty">{s.noChats}</p>}
        {p.list.map((c) => (
          <ConversationItem key={c.id} c={c} active={c.id === p.current} onOpen={() => p.onOpen(c.id)} onRenamed={p.onRenamed} onDeleted={p.onDeleted} />
        ))}
      </div>
      <div class="sidebar-foot">
        <span class="who" title={p.me.email}>
          {p.me.name || p.me.email}
        </span>
        <div class="lang" role="group" aria-label={s.language}>
          <button aria-pressed={locale() === "zh-HK"} onClick={() => p.onLocale("zh-HK")}>
            繁
          </button>
          <button aria-pressed={locale() === "en"} onClick={() => p.onLocale("en")}>
            EN
          </button>
        </div>
      </div>
    </nav>
  );
}

function ConversationItem(p: { c: ConversationSummary; active: boolean; onOpen: () => void; onRenamed: (id: string, t: string) => void; onDeleted: (id: string) => void }) {
  const s = t();
  const [menu, setMenu] = useState(false);
  const [editing, setEditing] = useState(false);
  const [confirmDel, setConfirmDel] = useState(false);
  const [title, setTitle] = useState(p.c.title);
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (editing) input.current?.focus();
  }, [editing]);

  const save = async () => {
    const v = title.trim();
    setEditing(false);
    setMenu(false);
    if (v && v !== p.c.title) {
      p.onRenamed(p.c.id, v);
      await api.update(p.c.id, { title: v }).catch(() => p.onRenamed(p.c.id, p.c.title));
    }
  };

  if (editing)
    return (
      <form
        class="item editing"
        role="listitem"
        onSubmit={(e) => {
          e.preventDefault();
          void save();
        }}
      >
        <input ref={input} value={title} aria-label={s.rename} onInput={(e) => setTitle((e.target as HTMLInputElement).value)} onBlur={() => void save()} onKeyDown={(e) => e.key === "Escape" && setEditing(false)} />
      </form>
    );

  return (
    <div class={`item ${p.active ? "active" : ""}`} role="listitem">
      <button class="item-main" onClick={p.onOpen} aria-current={p.active ? "page" : undefined}>
        <span class="item-title">{p.c.title || s.untitled}</span>
        <span class="item-meta">
          {p.c.running && <span class="running-dot" aria-label={s.working} />}
          {relativeTime(p.c.updatedAt)}
        </span>
      </button>
      <button class="icon-btn item-more" aria-label={s.conversationOptions} aria-expanded={menu} onClick={() => (setMenu(!menu), setConfirmDel(false))}>
        <Icon name="dots" size={18} />
      </button>
      {menu && (
        <div class="item-menu" role="menu">
          <button role="menuitem" onClick={() => (setTitle(p.c.title), setEditing(true))}>
            {s.rename}
          </button>
          {confirmDel ? (
            <button
              role="menuitem"
              class="danger"
              onClick={async () => {
                setMenu(false);
                await api.remove(p.c.id).then(() => p.onDeleted(p.c.id)).catch(() => {});
              }}
            >
              {s.confirmDelete}
            </button>
          ) : (
            <button role="menuitem" class="danger-text" onClick={() => setConfirmDel(true)}>
              {s.delete}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// ---- conversation ---------------------------------------------------------------------------------

interface Attachment {
  key: string;
  name: string;
  path?: string;
  progress: number;
  error?: boolean;
}

function ChatView(p: { me: Me; id: string | null; onCreated: (id: string) => void; onActivity: () => void; onTitle: (id: string, title: string) => void }) {
  const s = t();
  const [convId, setConvId] = useState<string | null>(p.id);
  const [conv, setConv] = useState<Conversation | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [turns, setTurns] = useState<Turn[]>([]);
  const [loadError, setLoadError] = useState(false);
  const [offline, setOffline] = useState(false);
  const [text, setText] = useState("");
  const [attachments, setAttachments] = useState<Attachment[]>([]);
  const [sendError, setSendError] = useState<string | null>(null);
  const [sending, setSending] = useState(false);
  const [model, setModel] = useState(p.me.defaultModel);
  const lastEvent = useRef(0);
  const creating = useRef<Promise<string> | null>(null);
  const lastTyping = useRef(0);
  const scroller = useRef<HTMLDivElement>(null);
  const stick = useRef(true);
  const textarea = useRef<HTMLTextAreaElement>(null);

  // load + stream
  useEffect(() => {
    if (!convId) return;
    let es: EventSource | null = null;
    let stopped = false;
    let backoff = 500;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const onEvent = (ev: MessageEvent) => {
      let e: ChatEvent;
      try {
        e = JSON.parse(ev.data);
      } catch {
        return;
      }
      if (typeof e.id === "number" && e.id > lastEvent.current) lastEvent.current = e.id;
      if (e.type === "title" && e.data?.title) {
        setConv((c) => (c ? { ...c, title: e.data.title } : c));
        p.onTitle(convId, e.data.title);
      }
      setTurns((ts) => mergeEvent(ts, e));
      if (e.type === "turn_finished") p.onActivity();
    };

    const connect = () => {
      if (stopped) return;
      es = new EventSource(api.streamUrl(convId, lastEvent.current));
      es.onopen = () => {
        backoff = 500;
        setOffline(false);
      };
      es.onmessage = onEvent;
      es.onerror = () => {
        es?.close();
        if (stopped) return;
        setOffline(true);
        timer = setTimeout(connect, backoff);
        backoff = Math.min(backoff * 2, 10_000);
      };
    };

    const load = async () => {
      try {
        const c = await api.conversation(convId);
        if (stopped) return;
        setConv(c);
        setModel(c.model || p.me.defaultModel);
        setMessages(c.messages);
        setTurns(c.turns);
        lastEvent.current = Math.max(0, ...c.turns.flatMap((tn) => tn.events.map((e) => e.id)));
        connect();
      } catch {
        if (!stopped) setLoadError(true);
      }
    };
    // Self-created (first keystroke of a new chat): nothing to load yet, just stream.
    if (creating.current) connect();
    else void load();

    const vis = () => {
      if (document.visibilityState === "visible" && es && es.readyState === EventSource.CLOSED) connect();
    };
    document.addEventListener("visibilitychange", vis);
    return () => {
      stopped = true;
      es?.close();
      if (timer) clearTimeout(timer);
      document.removeEventListener("visibilitychange", vis);
    };
  }, [convId]);

  // keep the view pinned to the bottom while new content arrives, unless the reader scrolled up
  useEffect(() => {
    const el = scroller.current;
    if (el && stick.current) el.scrollTop = el.scrollHeight;
  });
  const onScroll = () => {
    const el = scroller.current;
    if (el) stick.current = el.scrollHeight - el.scrollTop - el.clientHeight < 80;
  };

  const ensureConversation = (): Promise<string> => {
    if (convId) return Promise.resolve(convId);
    creating.current ??= api.create(model).then((r) => {
      setConvId(r.id);
      setConv({ id: r.id, title: "", model, messages: [], turns: [] });
      p.onCreated(r.id);
      return r.id;
    });
    creating.current.catch(() => (creating.current = null));
    return creating.current;
  };

  // typing: no conversation and no sandbox are made for it (a sandbox comes with the first bash command)
  const typing = () => {
    const now = Date.now();
    if (now - lastTyping.current < 30_000) return;
    lastTyping.current = now;
    api.typing(convId).catch(() => {});
  };

  const views = useMemo(() => turns.map(viewTurn), [turns]);
  const active = views.find((v) => isActive(v.status));

  const send = async (override?: string) => {
    const body = (override ?? text).trim();
    if (!body || sending || active) {
      if (active && body) setSendError(s.busy);
      return;
    }
    if (attachments.some((a) => !a.path && !a.error)) return;
    setSending(true);
    setSendError(null);
    stick.current = true;
    const paths = attachments.filter((a) => a.path).map((a) => a.path!);
    const local: Message = { id: -Date.now(), role: "user", content: body, attachments: paths, createdAt: new Date().toISOString(), turnId: null };
    setMessages((m) => [...m, local]);
    setText("");
    setAttachments([]);
    try {
      const id = await ensureConversation();
      const r = await api.send(id, body, paths);
      setMessages((m) => m.map((x) => (x.id === local.id ? { ...x, turnId: r.turnId } : x)));
      setTurns((ts) => (ts.some((x) => x.id === r.turnId) ? ts : [...ts, { id: r.turnId, status: "queued", startedAt: new Date().toISOString(), events: [] }]));
      p.onActivity();
    } catch (e) {
      setMessages((m) => m.filter((x) => x.id !== local.id));
      setText(body);
      setSendError((e as ApiError).status === 409 ? s.busy : s.sendFailed);
    } finally {
      setSending(false);
    }
  };

  const stop = () => {
    if (active) void api.cancel(active.id).catch(() => {});
  };

  const attach = async (fl: FileList | null) => {
    // copy now: the input is cleared right after this call returns (at the first await), which empties its FileList
    const picked = Array.from(fl ?? []);
    if (!picked.length) return;
    let id: string;
    try {
      id = await ensureConversation();
    } catch {
      setSendError(s.uploadFailed);
      return;
    }
    for (const f of picked) {
      const key = `${f.name}-${f.size}-${Math.random()}`;
      setAttachments((a) => [...a, { key, name: f.name, progress: 0 }]);
      api
        .upload(id, f, (pr) => setAttachments((a) => a.map((x) => (x.key === key ? { ...x, progress: pr } : x))))
        .then((r) => setAttachments((a) => a.map((x) => (x.key === key ? { ...x, path: r.path, progress: 1 } : x))))
        .catch(() => setAttachments((a) => a.map((x) => (x.key === key ? { ...x, error: true } : x))));
    }
  };

  const timeline = useMemo(() => timelineOf(messages, views), [messages, views]);
  const empty = timeline.length === 0;
  return (
    <div class="chat">
      {offline && (
        <div class="banner" role="status">
          {s.offline}
        </div>
      )}
      <div class="scroller" ref={scroller} onScroll={onScroll}>
        {loadError ? (
          <div class="center-note">{s.loadFailed}</div>
        ) : empty && !convId ? (
          <Empty me={p.me} />
        ) : (
          <div class="thread">
            {timeline.map((item) =>
              item.kind === "user" ? (
                <UserMessage key={`u${item.m.id}`} m={item.m} convId={convId!} />
              ) : item.kind === "plain" ? (
                <div key={`a${item.m.id}`} class="reply">
                  <div class="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(item.m.content) }} />
                </div>
              ) : (
                <Reply key={item.v.id} v={item.v} convId={convId!} />
              ),
            )}
          </div>
        )}
      </div>
      <Composer
        textarea={textarea}
        text={text}
        setText={(v) => {
          setText(v);
          setSendError(null);
          if (v) typing();
        }}
        attachments={attachments}
        removeAttachment={(key) => setAttachments((a) => a.filter((x) => x.key !== key))}
        onAttach={attach}
        onSend={() => void send()}
        onStop={stop}
        running={!!active}
        sending={sending}
        error={sendError}
        models={p.me.models}
        model={model}
        onModel={(m) => {
          setModel(m);
          if (convId) void api.update(convId, { model: m }).catch(() => {});
        }}
      />
    </div>
  );
}

// One draw per page load picks among the time slot's greetings.
const greetingDraw = Math.random();

// ---- the thread ----------------------------------------------------------------------------------

export type TimelineItem = { kind: "user"; m: Message } | { kind: "reply"; v: TurnView } | { kind: "plain"; m: Message };

/** Each user message, then its turn's reply (also a shared conversation's page). */
export function timelineOf(messages: Message[], views: TurnView[]): TimelineItem[] {
  const byTurn = new Map(views.map((v) => [v.id, v]));
  const used = new Set<string>();
  const out: TimelineItem[] = [];
  for (const m of messages) {
    if (m.role === "user") {
      out.push({ kind: "user", m });
      const v = m.turnId ? byTurn.get(m.turnId) : undefined;
      if (v) {
        out.push({ kind: "reply", v });
        used.add(v.id);
      }
    } else if (!m.turnId || !byTurn.get(m.turnId)?.blocks.length) {
      out.push({ kind: "plain", m });
      if (m.turnId) used.add(m.turnId);
    }
  }
  for (const v of views) if (!used.has(v.id)) out.push({ kind: "reply", v });
  return out;
}

function Empty({ me }: { me: Me }) {
  return (
    <div class="empty">
      <Logo me={me} />
      <h2>{greeting(locale(), new Date(), greetingDraw, me.firstName)}</h2>
    </div>
  );
}

const VIEWABLE = /\.(png|jpe?g|gif|webp)$/i;

export function UserMessage({ m, convId }: { m: Message; convId: string }) {
  return (
    <div class="user-msg">
      <div class="bubble">{m.content}</div>
      {!!m.attachments?.length && (
        <div class="file-row right">
          {m.attachments.map((path) =>
            VIEWABLE.test(path) ? (
              <a key={path} class="upload-thumb" href={api.fileUrl(convId, path, { inline: true })} target="_blank" rel="noopener" title={path.split("/").pop()}>
                <img src={api.fileUrl(convId, path, { inline: true })} alt={path.split("/").pop()} loading="lazy" />
              </a>
            ) : (
              <a key={path} class="file-chip" href={api.fileUrl(convId, path)} target="_blank" rel="noopener">
                <Icon name="file" size={16} />
                <span>{path.split("/").pop()}</span>
              </a>
            ),
          )}
        </div>
      )}
    </div>
  );
}

/** The files the agent presented (present_files), in its order: download cards, images and PDFs with a preview. */
function FileCards({ files, convId }: { files: PresentedFile[]; convId: string }) {
  const s = t();
  return (
    <div class="file-cards">
      {files.map((f) => {
        const name = f.path.split("/").pop() || f.path;
        const url = api.fileUrl(convId, f.path, { v: f.sha256 });
        const image = VIEWABLE.test(f.path);
        const pdf = /\.pdf$/i.test(f.path);
        return (
          <div key={f.path} class={`file-card ${image ? "is-image" : ""}`}>
            {(image || pdf) && (
              <a class="file-preview" href={image ? api.fileUrl(convId, f.path, { v: f.sha256, inline: true }) : url} target={image ? "_blank" : undefined} rel="noopener" download={image ? undefined : name}>
                <img
                  src={image ? api.fileUrl(convId, f.path, { v: f.sha256, inline: true }) : api.fileUrl(convId, f.path, { v: f.sha256, preview: true })}
                  alt={f.caption || name}
                  loading="lazy"
                  onError={(e) => ((e.currentTarget.parentElement as HTMLElement).style.display = "none")}
                />
              </a>
            )}
            <a class="file-chip made" href={url} download={name} aria-label={`${s.download}: ${name}`}>
              <Icon name="download" size={16} />
              <span class="file-name">{name}</span>
              <span class="file-size">{formatSize(f.size)}</span>
            </a>
            {f.caption && <p class="file-caption">{f.caption}</p>}
          </div>
        );
      })}
    </div>
  );
}

// ---- a reply: text, tool steps ----------------------------------------------------------------

export function Reply({ v, convId }: { v: TurnView; convId: string }) {
  const s = t();
  const running = isActive(v.status);
  const [copied, setCopied] = useState(false);

  const last = v.blocks[v.blocks.length - 1];
  const showWorking = running && (!last || last.kind !== "text" || v.thinking);
  const copy = async () => {
    const txt = replyText(v);
    try {
      await navigator.clipboard.writeText(txt);
    } catch {
      const ta = document.createElement("textarea");
      ta.value = txt;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
    }
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div class={`reply ${running ? "is-running" : ""}`} aria-live={running ? "polite" : undefined}>
      {groupBlocks(v.blocks).map((g, i) =>
        g.kind === "steps" ? (
          <div class="steps" key={`s${i}`}>
            {g.blocks.map((b) => (b.kind === "tool" ? <ToolStep key={b.id} b={b} convId={convId} /> : null))}
          </div>
        ) : g.block.kind === "text" ? (
          <div key={`t${i}`} class="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(g.block.text) }} />
        ) : g.block.kind === "notice" ? (
          <p key={`n${i}`} class="notice">
            {s.sandboxReset}
          </p>
        ) : g.block.kind === "error" ? (
          <p key={`e${i}`} class="inline-error">
            <Icon name="alert" size={16} /> {g.block.message}
          </p>
        ) : null,
      )}
      {showWorking && (
        <p class="working">
          <span class="pulse" aria-hidden="true" />
          {v.status === "queued" ? s.queued : v.sandboxStarting ? s.sandboxStarting : v.thinking ? s.thinking : s.working}
        </p>
      )}
      {v.status === "cancelled" && <p class="muted-note">{s.stopped}</p>}
      {v.status === "error" && v.budget && (
        <p class="inline-error is-budget" role="alert">
          <Icon name="alert" size={16} /> {s.budgetExhausted(usdAmount(v.budget.limitUsd), v.budget.ownerName)}
        </p>
      )}
      {v.status === "error" && !v.budget && (
        <p class="inline-error">
          <Icon name="alert" size={16} /> {s.failed}
          {v.error ? `: ${v.error}` : ""}
        </p>
      )}
      {v.presented.length > 0 && <FileCards files={v.presented} convId={convId} />}
      {!running && replyText(v) && (
        <div class="reply-actions">
          <button class="ghost-btn" onClick={copy} aria-label={s.copy}>
            <Icon name={copied ? "check" : "copy"} size={16} />
            <span>{copied ? s.copied : s.copy}</span>
          </button>
        </div>
      )}
    </div>
  );
}

type Group = { kind: "steps"; blocks: Block[] } | { kind: "one"; block: Block };
function groupBlocks(blocks: Block[]): Group[] {
  const out: Group[] = [];
  for (const b of blocks) {
    if (b.kind === "tool") {
      const g = out[out.length - 1];
      if (g && g.kind === "steps") g.blocks.push(b);
      else out.push({ kind: "steps", blocks: [b] });
    } else out.push({ kind: "one", block: b });
  }
  return out;
}

const formatSize = (n: number) => (n < 1024 ? `${n} B` : n < 1024 * 1024 ? `${(n / 1024).toFixed(0)} KB` : `${(n / 1024 / 1024).toFixed(1)} MB`);

function ToolStep({ b, convId }: { b: Extract<Block, { kind: "tool" }>; convId: string }) {
  const s = t();
  const [open, setOpen] = useState(false);
  const [label, detail] = describeTool(b.name, b.input);
  const thumbs = (b.images ?? []).slice(0, 4);
  return (
    <div class={`step ${b.done ? (b.isError ? "is-error" : "is-done") : "is-running"} ${open ? "open" : ""}`}>
      <button class="step-head" aria-expanded={open} onClick={() => setOpen(!open)}>
        <span class="step-icon">
          <Icon name={toolIcon(b.name)} size={16} />
        </span>
        <span class="step-label">{label}</span>
        {detail && <span class="step-detail">{detail}</span>}
        <span class="step-state" aria-hidden="true">
          {b.done ? b.isError ? <Icon name="alert" size={14} /> : null : <span class="spinner" />}
        </span>
        <span class="step-chev" aria-hidden="true">
          <Icon name="chevron" size={14} />
        </span>
      </button>
      {thumbs.length > 0 && (
        <div class="step-thumbs">
          {thumbs.map((im) => {
            const pdf = /\.pdf$/i.test(im.path);
            if (!pdf && !VIEWABLE.test(im.path)) return null;
            const src = api.fileUrl(convId, im.path, { v: im.sha256, preview: true, page: im.page });
            return (
              <a key={`${im.path}#${im.page ?? 1}`} href={src} target="_blank" rel="noopener" title={s.viewedImage}>
                <img src={src} alt={`${s.viewedImage}: ${im.path.split("/").pop()}`} loading="lazy" onError={(e) => (e.currentTarget.parentElement as HTMLElement).remove()} />
              </a>
            );
          })}
        </div>
      )}
      {open && (
        <div class="step-body">
          <StepPart title={s.input} value={b.input === undefined ? "" : typeof b.input === "string" ? b.input : JSON.stringify(b.input, null, 2)} />
          {b.output !== undefined && <StepPart title={s.output} value={b.output} error={b.isError} />}
        </div>
      )}
    </div>
  );
}

function StepPart({ title, value, error }: { title: string; value: string; error?: boolean }) {
  const s = t();
  const [all, setAll] = useState(false);
  const LIMIT = 3000;
  if (!value) return null;
  const long = value.length > LIMIT;
  return (
    <div class="step-part">
      <div class="step-part-title">{title}</div>
      <pre class={error ? "err" : ""}>{all || !long ? value : value.slice(0, LIMIT) + "\n…"}</pre>
      {long && !all && (
        <button class="link-btn" onClick={() => setAll(true)}>
          {s.showMore} ({Math.round(value.length / 1000)}k)
        </button>
      )}
    </div>
  );
}

// ---- composer -------------------------------------------------------------------------------------

function Composer(p: {
  textarea: { current: HTMLTextAreaElement | null };
  text: string;
  setText: (v: string) => void;
  attachments: Attachment[];
  removeAttachment: (key: string) => void;
  onAttach: (f: FileList | null) => void;
  onSend: () => void;
  onStop: () => void;
  running: boolean;
  sending: boolean;
  error: string | null;
  models: { id: string; label: string }[];
  model: string;
  onModel: (m: string) => void;
}) {
  const s = t();
  const fileInput = useRef<HTMLInputElement>(null);
  useEffect(() => {
    const ta = p.textarea.current;
    if (!ta) return;
    ta.style.height = "auto";
    ta.style.height = `${Math.min(ta.scrollHeight, Math.round(window.innerHeight * 0.4))}px`;
  }, [p.text]);
  const uploading = p.attachments.some((a) => !a.path && !a.error);
  const canSend = !!p.text.trim() && !p.sending && !uploading;
  return (
    <div class="composer-wrap">
      {p.error && (
        <p class="composer-error" role="alert">
          {p.error}
        </p>
      )}
      <form
        class="composer"
        onSubmit={(e) => {
          e.preventDefault();
          p.running ? p.onStop() : p.onSend();
        }}
      >
        {p.attachments.length > 0 && (
          <div class="attachments">
            {p.attachments.map((a) => (
              <span key={a.key} class={`attach-chip ${a.error ? "error" : ""}`}>
                <Icon name="file" size={14} />
                <span class="attach-name">{a.name}</span>
                {!a.path && !a.error && <span class="attach-progress">{Math.round(a.progress * 100)}%</span>}
                {a.error && <span class="attach-progress">{s.uploadFailed}</span>}
                <button type="button" class="attach-x" aria-label={`${s.removeAttachment}: ${a.name}`} onClick={() => p.removeAttachment(a.key)}>
                  <Icon name="close" size={14} />
                </button>
              </span>
            ))}
          </div>
        )}
        <textarea
          ref={p.textarea}
          rows={1}
          value={p.text}
          placeholder={s.composerPlaceholder}
          aria-label={s.composerPlaceholder}
          onInput={(e) => p.setText((e.target as HTMLTextAreaElement).value)}
          onKeyDown={(e) => {
            if (e.key !== "Enter" || e.shiftKey || e.isComposing || e.keyCode === 229 || coarse()) return;
            e.preventDefault();
            if (!p.running && canSend) p.onSend();
          }}
        />
        <div class="composer-bar">
          <button type="button" class="icon-btn" aria-label={s.attach} onClick={() => fileInput.current?.click()}>
            <Icon name="clip" />
          </button>
          <input
            ref={fileInput}
            type="file"
            multiple
            hidden
            onChange={(e) => {
              const el = e.target as HTMLInputElement;
              p.onAttach(el.files);
              el.value = "";
            }}
          />
          {showModelPicker(p.models) && (
            <label class="model-pick">
              <span class="visually-hidden">{s.model}</span>
              <select value={p.model} onChange={(e) => p.onModel((e.target as HTMLSelectElement).value)}>
                {p.models.map((m) => (
                  <option key={m.id} value={m.id}>
                    {m.label}
                  </option>
                ))}
              </select>
            </label>
          )}
          <span class="spacer" />
          {p.running ? (
            <button type="submit" class="send stop" aria-label={s.stop}>
              <Icon name="stop" size={18} />
            </button>
          ) : (
            <button type="submit" class="send" aria-label={s.send} disabled={!canSend}>
              <Icon name="send" size={18} />
            </button>
          )}
        </div>
      </form>
    </div>
  );
}

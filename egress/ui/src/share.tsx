// Sharing (docs/CHAT.md, "Sharing and continuing"): the share sheet of an open conversation (copy a link to a read-only
// snapshot; stop sharing earlier links) and the shared page at /s/<id>, read-only, whose one action is "continue
// conversation": a new conversation of the viewer's, which then works like any other.
import { useEffect, useMemo, useState } from "preact/hooks";
import { api, ApiError, shareRef, type Me, type ShareLink, type SharedConversation } from "./api.ts";
import { Reply, timelineOf, UserMessage } from "./app.tsx";
import { relativeTime, t } from "./i18n.ts";
import { Icon } from "./icons.tsx";
import { renderMarkdown } from "./md.ts";
import { viewTurn } from "./turns.ts";

export function shareRoute(): string | null {
  const m = /^\/s\/([A-Za-z0-9_-]{22})\/?$/.exec(location.pathname);
  return m ? m[1]! : null;
}

async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    const ta = document.createElement("textarea");
    ta.value = text;
    document.body.appendChild(ta);
    ta.select();
    document.execCommand("copy");
    ta.remove();
  }
}

export function ShareSheet({ me, convId, onClose }: { me: Me; convId: string; onClose: () => void }) {
  const s = t();
  const [links, setLinks] = useState<ShareLink[]>([]);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ ok: boolean; text: string } | null>(null);

  useEffect(() => {
    api.shares(convId).then(setLinks).catch(() => {});
    const esc = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    addEventListener("keydown", esc);
    return () => removeEventListener("keydown", esc);
  }, [convId]);

  // each copy freezes the conversation as it is now: a new link (later messages never show on an earlier one)
  const copy = async () => {
    setBusy(true);
    setNote(null);
    try {
      const l = await api.createShare(convId);
      await copyText(l.url);
      setLinks((ls) => [l, ...ls]);
      setNote({ ok: true, text: s.linkCopied });
    } catch (e) {
      setNote({ ok: false, text: (e as ApiError).status === 400 ? s.nothingToShare : s.copyLinkFailed });
    } finally {
      setBusy(false);
    }
  };

  const revoke = async (id: string) => {
    await api.revokeShare(id).then(() => setLinks((ls) => ls.filter((l) => l.id !== id))).catch(() => {});
  };

  return (
    <div class="sheet-scrim" onClick={(e) => e.target === e.currentTarget && onClose()}>
      <div class="sheet" role="dialog" aria-modal="true" aria-labelledby="share-title">
        <div class="sheet-head">
          <h2 id="share-title">{s.share}</h2>
          <button class="icon-btn" aria-label={s.close} onClick={onClose}>
            <Icon name="close" />
          </button>
        </div>
        <p class="sheet-lead">{s.shareLead(me.product.name)}</p>
        <button class="primary-btn" onClick={copy} disabled={busy}>
          <Icon name={note?.ok ? "check" : "copy"} size={18} />
          <span>{note?.ok ? s.linkCopied : s.copyLink}</span>
        </button>
        {note && !note.ok && (
          <p class="sheet-error" role="alert">
            {note.text}
          </p>
        )}
        {links.length > 0 && (
          <div class="share-links">
            <h3>{s.sharedLinks}</h3>
            {links.map((l) => (
              <div class="share-link" key={l.id}>
                <Icon name="share" size={16} />
                <span class="share-link-when">{relativeTime(l.createdAt)}</span>
                <button class="link-btn danger-text" onClick={() => void revoke(l.id)}>
                  {s.stopSharing}
                </button>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}

export function SharedView({ id, onTitle, onContinued }: { id: string; onTitle: (t: string) => void; onContinued: (id: string) => void }) {
  const s = t();
  const [conv, setConv] = useState<SharedConversation | null>(null);
  const [gone, setGone] = useState<"gone" | "failed" | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState(false);

  useEffect(() => {
    api
      .share(id)
      .then((c) => {
        setConv(c);
        onTitle(c.title);
      })
      .catch((e: ApiError) => setGone(e.status === 404 ? "gone" : "failed"));
  }, [id]);

  const views = useMemo(() => (conv ? conv.turns.map(viewTurn) : []), [conv]);
  const timeline = useMemo(() => (conv ? timelineOf(conv.messages, views) : []), [conv, views]);
  const ref = shareRef(id);

  const go = async () => {
    setBusy(true);
    setError(false);
    try {
      onContinued((await api.continueShare(id)).id);
    } catch {
      setError(true);
      setBusy(false);
    }
  };

  if (gone) return <div class="chat"><div class="center-note">{gone === "gone" ? s.shareGone : s.loadFailed}</div></div>;
  if (!conv) return <div class="chat" aria-busy="true" />;

  return (
    <div class="chat">
      <div class="scroller">
        <div class="thread">
          <p class="shared-note">
            <Icon name="lock" size={14} /> {s.sharedBy(conv.ownerName)}
          </p>
          {timeline.map((item) =>
            item.kind === "user" ? (
              <UserMessage key={`u${item.m.id}`} m={item.m} convId={ref} />
            ) : item.kind === "plain" ? (
              <div key={`a${item.m.id}`} class="reply">
                <div class="md" dangerouslySetInnerHTML={{ __html: renderMarkdown(item.m.content) }} />
              </div>
            ) : (
              <Reply key={item.v.id} v={item.v} convId={ref} />
            ),
          )}
        </div>
      </div>
      <div class="composer-wrap continue-bar">
        {error && (
          <p class="composer-error" role="alert">
            {s.continueFailed}
          </p>
        )}
        <button class="primary-btn wide" onClick={go} disabled={busy}>
          <Icon name="send" size={18} />
          <span>{s.continueConversation}</span>
        </button>
      </div>
    </div>
  );
}

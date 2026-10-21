// The operator page (docs/CHAT.md, "Operator page"): one self-contained HTML page (no build, no external assets) that
// polls ./snapshot every 10 s and renders it as cards. English (an operator page), proper nouns as recorded. Stacked
// rows, no tables: no horizontal scroll on a phone; on a wide screen the cards flow into columns.
export const OPS_PAGE = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>YYC AI Ops</title>
<style>
:root{--bg:#f6f6f4;--card:#fff;--fg:#1b1b1b;--mut:#6b6b6b;--line:#e4e4e0;--ok:#1f7a4d;--warn:#a15c00;--bad:#b3261e;--chip:#efefeb;--bar:#2f6fde}
@media (prefers-color-scheme:dark){:root{--bg:#121212;--card:#1c1c1c;--fg:#ececec;--mut:#9a9a9a;--line:#2c2c2c;--ok:#4cc38a;--warn:#e0a040;--bad:#ff6b5e;--chip:#2a2a2a;--bar:#6a9cff}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans HK",sans-serif}
header{position:sticky;top:0;z-index:1;background:var(--bg);border-bottom:1px solid var(--line);padding:10px 16px;display:flex;flex-wrap:wrap;gap:4px 14px;align-items:baseline}
header h1{font-size:17px;margin:0}
header .meta{color:var(--mut);font-size:12px}
main{padding:12px 16px 40px;display:grid;gap:12px;grid-template-columns:minmax(0,1fr)}
@media (min-width:900px){main{grid-template-columns:repeat(2,minmax(0,1fr))}.wide{grid-column:1/-1}}
@media (min-width:1400px){main{grid-template-columns:repeat(3,minmax(0,1fr))}}
section{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:12px 14px;min-width:0}
h2{font-size:14px;margin:0 0 8px;display:flex;gap:8px;align-items:baseline;flex-wrap:wrap}
h2 small{color:var(--mut);font-weight:400}
.kpis{display:flex;flex-wrap:wrap;gap:8px 18px;margin-bottom:8px}
.kpi b{display:block;font-size:20px;font-variant-numeric:tabular-nums}
.kpi span{color:var(--mut);font-size:12px}
.row{border-top:1px solid var(--line);padding:7px 0;display:flex;flex-wrap:wrap;gap:2px 12px;align-items:baseline;min-width:0}
.row:first-of-type{border-top:0}
.row .main{font-weight:600;min-width:0;overflow-wrap:anywhere}
.row .f{color:var(--mut);font-size:12.5px;overflow-wrap:anywhere}
.row .f b{color:var(--fg);font-weight:500}
.row .r{margin-left:auto;font-variant-numeric:tabular-nums}
.chip{display:inline-block;padding:0 7px;border-radius:9px;background:var(--chip);font-size:12px}
.ok{color:var(--ok)}.warn{color:var(--warn)}.bad{color:var(--bad)}
.bar{height:8px;border-radius:4px;background:var(--chip);overflow:hidden;margin:4px 0 8px}
.bar i{display:block;height:100%;background:var(--bar)}
.bar.over i{background:var(--bad)}
.empty{color:var(--mut);font-size:13px}
.err{color:var(--bad);font-size:12.5px;margin-top:6px}
code{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;overflow-wrap:anywhere}
#stale{display:none}
#stale.on{display:inline}
</style>
</head>
<body>
<header><h1>YYC AI Ops</h1><span class="meta" id="meta">loading…</span><span class="meta bad" id="stale">· not updating</span></header>
<main id="main"></main>
<script>
"use strict";
var TZ = (function(){ try { return Intl.DateTimeFormat().resolvedOptions().timeZone || "Asia/Hong_Kong"; } catch (e) { return "Asia/Hong_Kong"; } })();
function esc(s){ return String(s == null ? "" : s).replace(/[&<>"']/g, function(c){ return {"&":"&amp;","<":"&lt;",">":"&gt;","\\"":"&quot;","'":"&#39;"}[c]; }); }
function usd(v){ return v == null ? "–" : "USD " + Number(v).toFixed(Math.abs(v) < 1 ? 4 : 2); }
function dur(s){ if (s == null) return "–"; s = Math.max(0, Math.round(s)); if (s < 60) return s + " s"; if (s < 3600) return Math.floor(s/60) + " min"; if (s < 86400) return Math.floor(s/3600) + " h " + Math.floor(s%3600/60) + " min"; return Math.floor(s/86400) + " d " + Math.floor(s%86400/3600) + " h"; }
function ago(t){ return t ? dur((Date.now() - Date.parse(t)) / 1000) + " ago" : "–"; }
function when(t){ if (!t) return "–"; var d = new Date(t); return d.toLocaleString("en-GB", {timeZone: TZ, month: "short", day: "numeric", hour: "2-digit", minute: "2-digit", second: "2-digit"}); }
function short(id){ return id ? String(id).slice(0, 8) : "–"; }
function who(e){ return e ? esc(String(e).replace(/@.*/, "")) + '<span class="f">@' + esc(String(e).replace(/^[^@]*@/, "")) + "</span>" : "–"; }
function f(label, v){ return '<span class="f">' + esc(label) + " <b>" + v + "</b></span>"; }
function card(title, sub, body, err, wide){ return '<section' + (wide ? ' class="wide"' : "") + "><h2>" + esc(title) + (sub ? " <small>" + sub + "</small>" : "") + "</h2>" + body + (err ? '<div class="err">' + esc(err) + "</div>" : "") + "</section>"; }
function rows(list, fn, empty){ return list && list.length ? list.map(fn).join("") : '<div class="empty">' + esc(empty) + "</div>"; }
function statusClass(s){ return s === "active" || s === "ready" ? "ok" : s === "gone" ? "" : s === "retiring" || s === "booting" || s === "starting" || s === "releasing" ? "warn" : "bad"; }
function kpi(v, label){ return '<div class="kpi"><b>' + v + "</b><span>" + esc(label) + "</span></div>"; }

function render(d){
  var E = d.errors || {}, out = [];
  // budget
  var b = d.budget, st = b && b.status;
  var bb = "";
  if (b) {
    var t = b.total || {};
    if (st) {
      var pct = st.limitUsd > 0 ? Math.min(100, st.spentUsd / st.limitUsd * 100) : 100;
      bb += '<div class="kpis">' + kpi(usd(st.spentUsd), "spent of USD " + st.limitUsd + " since " + when(st.since)) + kpi(usd(st.remainingUsd), st.exhausted ? "left (used up: model calls refused)" : "left") + "</div>";
      bb += '<div class="bar' + (st.exhausted ? " over" : "") + '"><i style="width:' + pct.toFixed(1) + '%"></i></div>';
    } else bb += '<div class="empty">No budget configured.</div>';
    bb += '<div class="kpis">' + kpi(usd(t.today), "today (" + (t.callsToday || 0) + " calls)") + kpi(usd(t.month), "this month (" + (t.callsMonth || 0) + " calls)") + "</div>";
  }
  out.push(card("Model spend", "egress_log, " + esc(d.tz), bb, E.budget));
  out.push(card("Spend per person", "today · this month · since budget", rows(b && b.byUser, function(u){
    return '<div class="row"><span class="main">' + who(u.owner) + '</span><span class="r">' + usd(u.today) + " · " + usd(u.month) + " · " + usd(u.sinceBudget) + "</span></div>";
  }, "No model calls this month."), null));
  out.push(card("Spend per model", "today · this month · since budget", rows(b && b.byModel, function(m){
    return '<div class="row"><span class="main"><code>' + esc(m.model) + '</code></span><span class="r">' + usd(m.today) + " · " + usd(m.month) + " · " + usd(m.sinceBudget) + "</span>" + f("calls this month", m.callsMonth) + "</div>";
  }, "No model calls this month."), null));

  // sandboxes
  var s = d.sandboxes;
  var sb = "";
  if (s) {
    var st24 = s.starts24h || {};
    sb += '<div class="kpis">' + kpi(s.list.length + " / " + s.maxActive, "live / max") + kpi(st24.n, "starts 24 h (" + st24.failed + " failed)") + kpi(st24.p50Ms == null ? "–" : (st24.p50Ms/1000).toFixed(1) + " s", "start p50 (p95 " + (st24.p95Ms == null ? "–" : (st24.p95Ms/1000).toFixed(1) + " s") + ")") + "</div>";
    sb += rows(s.list, function(x){
      return '<div class="row"><span class="main">' + who(x.owner) + '</span><span class="chip ' + statusClass(x.status) + '">' + esc(x.status) + "</span>" +
        f("conversation", "<code>" + esc(x.conversationId) + "</code>") + f("host", esc(x.host || "–")) + f("age", dur(x.ageS)) + f("last active", dur(x.idleS) + " ago") + (x.migrations ? f("moves", x.migrations) : "") + "</div>";
    }, "No live sandboxes.");
    if (s.releases24h && s.releases24h.length) sb += '<div class="row">' + f("released 24 h", s.releases24h.map(function(r){ return esc(r.reason) + " " + r.n; }).join(", ")) + "</div>";
  }
  out.push(card("Live sandboxes", s ? "idle " + s.idleMinutes + " min, max " + s.maxLifetimeHours + " h" : "", sb, E.sandboxes, true));

  // hosts
  var h = d.hosts;
  out.push(card("Sandbox hosts", h ? (h.maxHosts != null ? "AWS max " + h.maxHosts + " · " : "") + h.gone24h + " gone in 24 h (latest shown)" : "", h ? rows(h.list, function(x){
    var kind = x.gateway ? "gateway" : x.kind;
    return '<div class="row"><span class="main">' + esc(x.name) + '</span><span class="chip">' + esc(kind) + '</span><span class="chip ' + statusClass(x.status) + '">' + esc(x.status) + "</span>" +
      (x.status !== "gone" ? '<span class="' + (x.reachable ? "ok" : "bad") + ' f">' + (x.reachable ? "answering" : "not answering") + "</span>" : "") +
      (x.gateway ? "" : f("sandboxes", x.sandboxes + (x.slots != null ? " / " + x.slots : ""))) +
      f("launched", when(x.launchedAt)) + (x.instanceId ? f("instance", "<code>" + esc(x.instanceId) + "</code>") : "") +
      (x.load1 != null ? f("load", Number(x.load1).toFixed(2)) : "") + (x.memAvailable != null && x.memTotal ? f("mem free", Math.round(x.memAvailable / 1048576) + " / " + Math.round(x.memTotal / 1048576) + " MiB") : "") +
      (x.retiring ? '<span class="warn f">retiring: sandboxes move off, then terminated</span>' : "") +
      (x.emptySince && x.status !== "gone" ? f("empty since", when(x.emptySince)) : "") +
      (x.goneAt ? f("gone", when(x.goneAt) + (x.goneReason ? " (" + esc(x.goneReason) + ")" : "")) : "") + "</div>";
  }, "No hosts.") : "", E.hosts));

  // scaler
  var sc = d.scaler, scb = "";
  if (sc) {
    var p = sc.plan;
    if (p) scb += '<div class="kpis">' + kpi(p.need, "need (demand " + p.demand + ", waiting " + p.waiting + ")") + kpi(p.available, "free slots (ready)") + kpi(p.pending, "slots booting") + kpi(p.perHost, "slots per host") + "</div>";
    scb += rows(sc.hints, function(x){ return '<div class="row"><span class="main">' + who(x.who) + "</span>" + f("typed", ago(x.at)) + (x.counted ? '<span class="chip ok">counted</span>' : '<span class="chip">expired</span>') + "</div>"; }, "No warm hints.");
  }
  out.push(card("Scaler", sc ? "demand hints count for " + sc.windowS + " s" : "", scb, E.scaler));

  // people
  out.push(card("Conversations per staff member", "counted from sandboxes (first message)", rows(d.people, function(u){
    return '<div class="row"><span class="main">' + who(u.owner) + (u.former ? ' <span class="chip">not a user now</span>' : "") + "</span>" + f("total", u.total) + f("today", u.today) + f("7 days", u.last7d) + f("live", u.live) + f("deleted", u.deleted) + f("last active", ago(u.lastActiveAt)) + "</div>";
  }, "No users."), E.people));

  // egress errors
  var e = d.egress, eb = "";
  if (e) {
    eb += rows(e.byStatus, function(x){ return '<div class="row"><span class="main">' + esc(x.kind) + " " + esc(x.status) + "</span>" + f("decision", esc(x.decision)) + f("count", x.n) + f("last", ago(x.lastAt)) + "</div>"; }, "No model or route errors in 24 h.");
    eb += "<h2 style=\\"margin-top:10px\\">Blocked hosts <small>24 h</small></h2>" + rows(e.deniedHosts, function(x){ return '<div class="row"><span class="main"><code>' + esc(x.host) + "</code></span>" + f("why", esc(x.why || "deny")) + f("count", x.n) + f("last", ago(x.lastAt)) + "</div>"; }, "Nothing blocked in 24 h.");
    eb += "<h2 style=\\"margin-top:10px\\">Latest refusals and errors</h2>" + rows(e.recent, function(x){ return '<div class="row"><span class="main">' + (x.status != null ? esc(x.status) : "–") + " <code>" + esc(x.host) + "</code></span>" + f("decision", esc(x.decision)) + f("who", who(x.owner)) + f("at", when(x.at)) + "</div>"; }, "None in 24 h.");
  }
  out.push(card("Egress errors", "last 24 h", eb, E.egress, true));

  // vm events
  out.push(card("Host and sandbox events", "vm_events, latest 60", rows(d.events, function(x){
    var data = x.data && Object.keys(x.data).length ? JSON.stringify(x.data) : "";
    var bad = /error|lost|fail/.test(x.kind) || (x.data && x.data.error);
    return '<div class="row"><span class="main ' + (bad ? "bad" : "") + '">' + esc(x.kind) + "</span>" + f("at", when(x.at)) + (x.host ? f("host", esc(x.host)) : "") + (x.sandboxId ? f("sandbox", short(x.sandboxId) + (x.owner ? " · " + who(x.owner) : "")) : "") + (data ? '<span class="f"><code>' + esc(data.length > 400 ? data.slice(0, 400) + "…" : data) + "</code></span>" : "") + "</div>";
  }, "No events."), E.events, true));

  document.getElementById("main").innerHTML = out.join("");
  document.getElementById("meta").textContent = "updated " + when(d.at) + " · " + d.tz + " · provider " + d.provider + " · image " + String(d.sandboxImage || "").split("/").pop();
}

var last = 0;
function load(){
  fetch("snapshot?tz=" + encodeURIComponent(TZ), {cache: "no-store", credentials: "same-origin"}).then(function(r){
    if (!r.ok) throw new Error("HTTP " + r.status);
    return r.json();
  }).then(function(d){ last = Date.now(); render(d); document.getElementById("stale").className = "meta bad"; })
    .catch(function(){ if (Date.now() - last > 30000) document.getElementById("stale").className = "meta bad on"; });
}
load();
setInterval(function(){ if (!document.hidden) load(); }, 10000);
document.addEventListener("visibilitychange", function(){ if (!document.hidden) load(); });
</script>
</body>
</html>
`;

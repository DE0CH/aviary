// The operator page (docs/CHAT.md, "Operator page"): one self-contained HTML page (no build, no external assets) that
// polls ./snapshot every 10 s. Two halves, never mixed: NOW (is everything fine: the verdict and its reasons, the
// service's pods, the gateway, hosts that exist, live sandboxes, the scaler, the budget, errors of the last 15 minutes)
// and HISTORY (a log, newest first: sandbox starts and releases, host and sandbox events, hosts gone, egress errors over
// 24 h, spend per day/person/model, conversations per staff member with a 「View as」 button each, the view-as log).
// Phones get the two as tabs, wide screens side by side. 「View as」 is a form POST to the UI router's
// /api/jarvis/view-as in a new tab (docs/CHAT.md "View as"). English (an operator page), proper nouns as recorded, times in Hong Kong time. Stacked rows, no tables.
// The script sits in a template literal: no dollar-brace, and backslashes doubled.
export const OPS_PAGE = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="robots" content="noindex">
<title>YYC AI Ops</title>
<style>
:root{--bg:#f5f5f2;--card:#fff;--fg:#1b1b1b;--mut:#6b6b6b;--line:#e4e4e0;--ok:#1f7a4d;--okbg:#e3f3ea;--warm:#8a5a00;--warmbg:#fbf0d9;--bad:#b3261e;--badbg:#fbe4e1;--chip:#efefeb;--bar:#2f6fde;--hist:#ecebe6}
@media (prefers-color-scheme:dark){:root{--bg:#121212;--card:#1c1c1c;--fg:#ececec;--mut:#9a9a9a;--line:#2e2e2e;--ok:#4cc38a;--okbg:#15301f;--warm:#e6b04a;--warmbg:#33270f;--bad:#ff7a6e;--badbg:#3a1714;--chip:#2a2a2a;--bar:#6a9cff;--hist:#181818}}
*{box-sizing:border-box}
body{margin:0;background:var(--bg);color:var(--fg);font:14px/1.45 system-ui,-apple-system,"Segoe UI",Roboto,"Noto Sans HK",sans-serif}
header{position:sticky;top:0;z-index:2;background:var(--bg);border-bottom:1px solid var(--line);padding:10px 16px 0}
.top{display:flex;flex-wrap:wrap;gap:6px 12px;align-items:center}
header h1{font-size:17px;margin:0}
.meta{color:var(--mut);font-size:12px}
.pill{display:inline-flex;align-items:center;gap:6px;padding:3px 10px;border-radius:14px;font-weight:600;font-size:13px}
.pill::before{content:"";width:9px;height:9px;border-radius:50%;background:currentColor}
.s-ok{color:var(--ok);background:var(--okbg)}.s-warming,.s-watch{color:var(--warm);background:var(--warmbg)}.s-problem{color:var(--bad);background:var(--badbg)}.s-none{color:var(--mut);background:var(--chip)}
.tabs{display:flex;gap:4px;margin-top:8px}
.tabs button{flex:1;border:0;background:none;color:var(--mut);font:600 14px system-ui,sans-serif;padding:8px 0;border-bottom:3px solid transparent;cursor:pointer}
.tabs button.on{color:var(--fg);border-bottom-color:var(--fg)}
#stale{display:none}#stale.on{display:inline}
main{padding:12px 16px 40px;display:grid;gap:16px;grid-template-columns:minmax(0,1fr)}
.col{display:grid;gap:12px;align-content:start;min-width:0}
.colhead{display:flex;flex-wrap:wrap;gap:4px 10px;align-items:baseline;padding:2px 2px 0}
.colhead h2{margin:0;font-size:15px;letter-spacing:.02em;text-transform:uppercase}
.colhead span{color:var(--mut);font-size:12.5px}
#history section{background:var(--hist)}
body.tab-now #history,body.tab-history #now{display:none}
@media (min-width:1100px){
  .tabs{display:none}
  main{grid-template-columns:minmax(0,1fr) minmax(0,1fr);gap:20px}
  body.tab-now #history,body.tab-history #now{display:grid}
  #history{border-left:1px solid var(--line);padding-left:20px}
}
section{background:var(--card);border:1px solid var(--line);border-radius:10px;padding:12px 14px;min-width:0}
h3{font-size:14px;margin:0 0 8px;display:flex;gap:8px;align-items:baseline;flex-wrap:wrap}
h3 small{color:var(--mut);font-weight:400}
h4{font-size:13px;margin:12px 0 4px;color:var(--mut);font-weight:600}
.kpis{display:flex;flex-wrap:wrap;gap:8px 20px;margin-bottom:8px}
.kpi b{display:block;font-size:20px;font-variant-numeric:tabular-nums}
.kpi span{color:var(--mut);font-size:12px}
.row{border-top:1px solid var(--line);padding:7px 0;display:flex;flex-wrap:wrap;gap:2px 12px;align-items:baseline;min-width:0}
.row:first-child,h3+.row,h4+.row,.kpis+.row{border-top:0}
.main{font-weight:600;min-width:0;overflow-wrap:anywhere}
.f{color:var(--mut);font-size:12.5px;overflow-wrap:anywhere}
.f b{color:var(--fg);font-weight:500}
.r{margin-left:auto;font-variant-numeric:tabular-nums}
.t{font-variant-numeric:tabular-nums;color:var(--mut);font-size:12.5px;min-width:9.5em}
.chip{display:inline-block;padding:0 8px;border-radius:9px;background:var(--chip);font-size:12px;font-weight:600}
.chip.s-ok,.chip.s-warming,.chip.s-watch,.chip.s-problem{font-weight:600}
.dot{display:inline-block;width:8px;height:8px;border-radius:50%;margin-right:4px;vertical-align:middle;background:var(--mut)}
.dot.ok{background:var(--ok)}.dot.warming,.dot.watch{background:var(--warm)}.dot.problem{background:var(--bad)}
.find{display:flex;gap:8px;align-items:flex-start;padding:6px 0;border-top:1px solid var(--line)}
.find:first-child{border-top:0}
.find .dot{margin-top:6px;flex:none}
.find .lv{font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.04em;flex:none;min-width:5.5em}
.lv.problem{color:var(--bad)}.lv.warming,.lv.watch{color:var(--warm)}
.allok{color:var(--ok);font-weight:600}
.pods{display:flex;flex-wrap:wrap;gap:4px 6px;width:100%}
.pod{font-size:12px;padding:1px 7px;border-radius:8px;background:var(--chip)}
.bar{height:8px;border-radius:4px;background:var(--chip);overflow:hidden;margin:4px 0 8px}
.bar i{display:block;height:100%;background:var(--bar)}
.bar.watch i{background:var(--warm)}.bar.over i{background:var(--bad)}
.mini{display:inline-block;width:70px;height:6px;border-radius:3px;background:var(--chip);overflow:hidden;vertical-align:middle}
.mini i{display:block;height:100%;background:var(--bar)}
.empty{color:var(--mut);font-size:13px}
.err{color:var(--bad);font-size:12.5px;margin-top:6px}
.bad{color:var(--bad)}.okc{color:var(--ok)}.warmc{color:var(--warm)}
code{font:12px ui-monospace,SFMono-Regular,Menlo,monospace;overflow-wrap:anywhere}
form.va{margin:0 0 0 auto}
form.va button{font:600 12px system-ui,sans-serif;padding:3px 10px;border-radius:12px;border:1px solid var(--line);background:var(--card);color:var(--fg);cursor:pointer}
form.va button:hover{border-color:var(--fg)}
</style>
</head>
<body class="tab-now">
<header>
  <div class="top"><h1>YYC AI Ops</h1><span class="pill s-none" id="verdict">loading…</span><span class="meta" id="meta"></span><span class="meta bad" id="stale">· not updating</span></div>
  <nav class="tabs"><button id="t-now" class="on" onclick="tab('now')">Now</button><button id="t-history" onclick="tab('history')">History (log)</button></nav>
</header>
<main>
  <div class="col" id="now"><div class="colhead"><h2>Now</h2><span>the current state, refreshed every 10 s</span></div><div class="col" id="now-body"></div></div>
  <div class="col" id="history"><div class="colhead"><h2>History</h2><span>a log: newest first, Hong Kong time</span></div><div class="col" id="history-body"></div></div>
</main>
<script>
"use strict";
var TZ = "Asia/Hong_Kong";
var LABEL = {ok: "All fine", warming: "Warming up", watch: "Needs a look", problem: "Problem"};
var LEVEL = {warming: "Warming", watch: "Watch", problem: "Problem"};
function tab(t){
  document.body.className = "tab-" + t;
  document.getElementById("t-now").className = t === "now" ? "on" : "";
  document.getElementById("t-history").className = t === "history" ? "on" : "";
  try { history.replaceState(null, "", t === "history" ? "#history" : "#now"); } catch (e) {}
}
if (location.hash === "#history") tab("history");
function esc(s){ return String(s == null ? "" : s).replace(/[&<>"']/g, function(c){ return {"&":"&amp;","<":"&lt;",">":"&gt;","\\"":"&quot;","'":"&#39;"}[c]; }); }
function usd(v){ return v == null ? "–" : "USD " + Number(v).toFixed(Math.abs(v) < 1 && v != 0 ? 4 : 2); }
function dur(s){ if (s == null) return "–"; s = Math.max(0, Math.round(s)); if (s < 60) return s + " s"; if (s < 3600) return Math.floor(s/60) + " min"; if (s < 86400) return Math.floor(s/3600) + " h " + Math.floor(s%3600/60) + " min"; return Math.floor(s/86400) + " d " + Math.floor(s%86400/3600) + " h"; }
function secs(ms){ return ms == null ? "–" : (ms < 10000 ? (ms/1000).toFixed(1) : Math.round(ms/1000)) + " s"; }
function ago(t){ return t ? dur((Date.now() - Date.parse(t)) / 1000) + " ago" : "–"; }
var DAYFMT = new Intl.DateTimeFormat("en-GB", {timeZone: TZ, year: "numeric", month: "2-digit", day: "2-digit"});
function when(t){
  if (!t) return "–";
  var d = new Date(t), today = DAYFMT.format(new Date()) === DAYFMT.format(d);
  return d.toLocaleString("en-GB", today ? {timeZone: TZ, hour: "2-digit", minute: "2-digit", second: "2-digit"} : {timeZone: TZ, month: "short", day: "numeric", hour: "2-digit", minute: "2-digit"});
}
function short(id){ return id ? String(id).slice(0, 8) : "–"; }
function who(e){ return e ? esc(String(e).replace(/@.*/, "")) + '<span class="f">@' + esc(String(e).replace(/^[^@]*@/, "")) + "</span>" : '<span class="f">unknown</span>'; }
function f(label, v){ return '<span class="f">' + esc(label) + " <b>" + v + "</b></span>"; }
function chip(level, text){ return '<span class="chip s-' + level + '">' + esc(text) + "</span>"; }
function card(title, sub, body, err){ return "<section><h3>" + esc(title) + (sub ? " <small>" + sub + "</small>" : "") + "</h3>" + body + (err ? '<div class="err">Could not read this part: ' + esc(err) + "</div>" : "") + "</section>"; }
function rows(list, fn, empty){ return list && list.length ? list.map(fn).join("") : '<div class="empty">' + esc(empty) + "</div>"; }
function kpi(v, label){ return '<div class="kpi"><b>' + v + "</b><span>" + esc(label) + "</span></div>"; }
function t(at){ return '<span class="t">' + when(at) + "</span>"; }
function hostLevel(x){ if (x.status === "booting" || x.status === "retiring" || x.status === "draining") return "warming"; return x.reachable ? "ok" : "problem"; }
function hostWord(x){
  if (x.status === "booting") return "booting";
  if (x.status === "retiring") return "retiring";
  if (x.status === "draining") return "draining";
  if (!x.reachable) return "not answering";
  return x.sandboxes ? "running" : "ready, empty";
}
function mib(b){ return Math.round(b / 1048576).toLocaleString("en-GB"); }

function renderNow(d){
  var E = d.errors || {}, out = [], H = d.health || {state: "ok", findings: []};
  // 1. the verdict and its reasons
  out.push(card("Status", "", H.findings.length ? H.findings.map(function(x){
    return '<div class="find"><span class="dot ' + x.level + '"></span><span class="lv ' + x.level + '">' + esc(LEVEL[x.level]) + "</span><span>" + esc(x.text) + "</span></div>";
  }).join("") : '<div class="allok">Everything is running normally.</div>', null));

  // 2. the service: pods of each role, and the gateway
  var svc = d.service, sb = "";
  if (svc && svc.available) {
    sb += svc.deployments.map(function(x){
      var lv = x.state, word = x.missing ? "missing" : x.ready + " / " + x.desired + " ready" + (x.rolling ? ", rolling out" : "");
      return '<div class="row"><span class="main">' + esc(x.name) + "</span>" + chip(lv, word) + '<div class="pods">' + (x.pods || []).map(function(p){
        return '<span class="pod"><span class="dot ' + (p.ready ? "ok" : p.phase === "Terminating" ? "warming" : "problem") + '"></span>' + esc(p.name.split("-").pop()) + " · " + esc(p.ready ? "ready" : p.phase) + " · " + dur(p.ageS) + (p.restarts ? ' · <b class="warmc">' + p.restarts + " restarts</b>" : "") + (p.node ? " · " + esc(String(p.node).replace(/^yyc-pool-/, "")) : "") + "</span>";
      }).join("") + "</div></div>";
    }).join("");
  } else if (svc) sb += '<div class="empty">Pod status is not available here (not in a cluster).</div>';
  var gw = d.hosts && d.hosts.list.filter(function(h){ return h.gateway; })[0];
  if (gw) sb += '<div class="row"><span class="main">gateway ' + esc(gw.name) + "</span>" + chip(gw.reachable ? "ok" : "problem", gw.reachable ? "answering" : "not answering") + f("last seen", ago(gw.lastSeenAt)) + (gw.gvisor ? f("gVisor", esc(gw.gvisor)) : "") + "</div>";
  else if (d.gatewayMissing) sb += '<div class="row"><span class="main bad">No gateway registered</span></div>';
  out.push(card("Service", "pods Ready per role", sb, E.service || E.hosts));

  // 3. sandbox hosts that exist now
  var h = d.hosts, hb = "";
  if (h) {
    var aws = h.list.filter(function(x){ return !x.gateway; });
    hb += rows(aws, function(x){
      var used = x.slots ? Math.min(100, x.sandboxes / x.slots * 100) : 0;
      return '<div class="row"><span class="main">' + esc(x.name) + "</span>" + chip(hostLevel(x), hostWord(x)) +
        f("sandboxes", x.sandboxes + (x.slots != null ? " / " + x.slots + ' <span class="mini"><i style="width:' + used.toFixed(0) + '%"></i></span>' : "")) +
        (x.memAvailable != null && x.memTotal ? f("memory free", mib(x.memAvailable) + " / " + mib(x.memTotal) + " MiB") : "") +
        (x.load1 != null ? f("load", Number(x.load1).toFixed(2) + (x.cpus ? " on " + x.cpus + " CPUs" : "")) : "") +
        f("up", dur((Date.now() - Date.parse(x.launchedAt)) / 1000) + " (since " + when(x.launchedAt) + ")") +
        (x.maxSandboxes != null ? f("capped at", x.maxSandboxes) : "") +
        (x.emptySince ? f("empty since", when(x.emptySince)) : "") +
        (x.instanceId ? f("instance", "<code>" + esc(x.instanceId) + "</code>") : "") + (x.gvisor ? f("gVisor", esc(x.gvisor)) : "") + "</div>";
    }, "No sandbox hosts running now. The scaler starts one when someone types or opens a conversation.");
  }
  out.push(card("Sandbox hosts", h && h.maxHosts != null ? "running now · at most " + h.maxHosts + " (hosts gone: History)" : "running now", hb, E.hosts));

  // 4. live sandboxes
  var s = d.sandboxes, lb = "";
  if (s) {
    lb += '<div class="kpis">' + kpi(s.list.length + " / " + s.maxActive, "live / max") + kpi(s.idleMinutes + " min", "released when idle") + "</div>";
    lb += rows(s.list, function(x){
      var lv = x.status === "ready" ? "ok" : x.status === "starting" || x.status === "releasing" ? "warming" : "problem";
      return '<div class="row"><span class="main">' + who(x.owner) + "</span>" + chip(lv, x.status) + f("host", esc(x.host || "–")) + f("up", dur(x.ageS)) + f("last used", dur(x.idleS) + " ago") + f("conversation", "<code>" + short(x.conversationId) + "</code>") + (x.migrations ? f("moved", x.migrations + "×") : "") + "</div>";
    }, "No live sandboxes.");
  }
  out.push(card("Live sandboxes", "who has one now", lb, E.sandboxes));

  // 5. the scaler
  var sc = d.scaler, scb = "";
  if (sc) {
    var p = sc.plan;
    if (p) {
      var verdict = p.want > 0 ? '<span class="warmc">wants ' + p.want + " more slot" + (p.want > 1 ? "s" : "") + "</span>" : '<span class="okc">has enough</span>';
      scb += '<div class="row"><span class="main">Plan: ' + verdict + "</span>" + f("needs", p.need + " slot" + (p.need === 1 ? "" : "s")) + f("people typing", p.demand) + f("waiting for a sandbox", p.waiting) + f("free on ready hosts", p.available) + f("booting", p.pending) + f("per host", p.perHost) + "</div>";
    } else scb += '<div class="empty">No scaler (provider ' + esc(d.provider) + ").</div>";
    scb += "<h4>Warm hints counted now (typing in the last " + sc.windowS + " s)</h4>" + rows(sc.hints, function(x){ return '<div class="row"><span class="main">' + who(x.who) + "</span>" + f("typed", ago(x.at)) + "</div>"; }, "Nobody is typing.");
  }
  out.push(card("Scaler", "", scb, E.scaler));

  // 6. budget
  var b = d.budget, st = b && b.status, bb = "";
  if (b) {
    var tt = b.total || {};
    if (st) {
      var frac = st.limitUsd > 0 ? st.spentUsd / st.limitUsd : 1, pct = Math.min(100, frac * 100);
      bb += '<div class="kpis">' + kpi(usd(st.spentUsd), "used of USD " + st.limitUsd + " (" + pct.toFixed(0) + "%)") + kpi(usd(st.remainingUsd), st.exhausted ? "left: used up, model calls refused" : "left") + "</div>";
      bb += '<div class="bar' + (st.exhausted ? " over" : frac >= 0.8 ? " watch" : "") + '"><i style="width:' + pct.toFixed(1) + '%"></i></div>';
      bb += '<div class="f">counting since ' + when(st.since) + "</div>";
    } else bb += '<div class="empty">No budget configured.</div>';
    bb += '<div class="kpis" style="margin-top:8px">' + kpi(usd(tt.today), "today (" + (tt.callsToday || 0) + " calls)") + kpi(usd(tt.month), "this month (" + (tt.callsMonth || 0) + " calls)") + "</div>";
  }
  out.push(card("Model budget", "shared by everyone; per day, person and model: History", bb, E.budget));

  // 7. errors of the last 15 minutes
  var r = d.recent, rb = "";
  if (r) {
    rb += rows(r.list, function(x){ return '<div class="row">' + t(x.at) + '<span class="main bad">' + esc(x.what) + "</span>" + (x.detail ? f("", esc(x.detail)) : "") + (x.owner ? f("who", who(x.owner)) : "") + "</div>"; }, "No errors in the last " + d.recentMinutes + " minutes.");
    if (r.blocked) rb += '<div class="row">' + f("blocked internet requests (normal; listed in History)", r.blocked) + "</div>";
  }
  out.push(card("Current errors", "last " + d.recentMinutes + " min; the 24 h log is in History", rb, E["recent errors"]));
  return out.join("");
}

function renderHistory(d){
  var E = d.errors || {}, out = [];
  // sandbox starts and releases
  var s = d.starts, sb = "";
  if (s) {
    var m = s.summary;
    sb += '<div class="kpis">' + kpi(m.n, "starts (" + m.failed + " failed)") + kpi(m.p50Ms == null ? "–" : secs(m.p50Ms), "start time p50") + kpi(m.p95Ms == null ? "–" : secs(m.p95Ms), "p95") + "</div>";
    if (s.releases && s.releases.length) sb += '<div class="row">' + f("released", s.releases.map(function(x){ return esc(x.reason) + " " + x.n; }).join(", ")) + "</div>";
    sb += rows(s.list, function(x){
      var what = x.what === "start" ? (x.ok ? '<span class="main okc">start</span>' + f("took", secs(x.ms)) : '<span class="main bad">start failed</span>' + f("after", secs(x.ms)) + f("why", esc(x.reason || ""))) :
        '<span class="main">release</span>' + f("lived", dur(x.ms / 1000)) + f("why", esc(x.reason || "?"));
      return '<div class="row">' + t(x.at) + what + f("who", who(x.owner)) + (x.host ? f("host", esc(x.host)) : "") + "</div>";
    }, "No sandbox starts or releases in 24 h.");
  }
  out.push(card("Sandbox starts and releases", "24 h", sb, E["sandbox starts"]));

  // vm events
  out.push(card("Host and sandbox events", "vm_events, latest 60", rows(d.events, function(x){
    var data = x.data && Object.keys(x.data).length ? JSON.stringify(x.data) : "";
    var bad = /error|lost|fail/.test(x.kind) || (x.data && x.data.error);
    return '<div class="row">' + t(x.at) + '<span class="main ' + (bad ? "bad" : "") + '">' + esc(x.kind) + "</span>" + (x.host ? f("host", esc(x.host)) : "") + (x.sandboxId ? f("sandbox", short(x.sandboxId) + (x.owner ? " · " + who(x.owner) : "")) : "") + (data ? '<span class="f"><code>' + esc(data.length > 300 ? data.slice(0, 300) + "…" : data) + "</code></span>" : "") + "</div>";
  }, "No events."), E.events));

  // hosts gone
  var g = d.hostsGone;
  out.push(card("Hosts gone", g ? "24 h: " + g.count24h + (g.count24h > g.list.length ? " (latest " + g.list.length + " shown)" : "") : "", g ? rows(g.list, function(x){
    return '<div class="row">' + t(x.goneAt) + '<span class="main">' + esc(x.name) + "</span>" + f("why", esc(x.goneReason || "?")) + f("lived", dur(x.livedS)) + f("launched", when(x.launchedAt)) + (x.instanceId ? f("instance", "<code>" + esc(x.instanceId) + "</code>") : "") + "</div>";
  }, "No hosts gone in 24 h.") : "", E["hosts gone"]));

  // egress errors
  var e = d.egress, eb = "";
  if (e) {
    eb += "<h4>Latest refusals and errors</h4>" + rows(e.recent, function(x){ return '<div class="row">' + t(x.at) + '<span class="main">' + (x.status != null ? esc(x.status) : "–") + " <code>" + esc(x.host) + "</code></span>" + f("decision", esc(x.decision)) + f("who", who(x.owner)) + "</div>"; }, "None in 24 h.");
    eb += "<h4>Model and route errors by kind</h4>" + rows(e.byStatus, function(x){ return '<div class="row"><span class="main">' + esc(x.kind) + " " + esc(x.status) + "</span>" + f("decision", esc(x.decision)) + f("count", x.n) + f("last", when(x.lastAt)) + "</div>"; }, "No model or route errors in 24 h.");
    eb += "<h4>Blocked hosts</h4>" + rows(e.deniedHosts, function(x){ return '<div class="row"><span class="main"><code>' + esc(x.host) + "</code></span>" + f("why", esc(x.why || "deny")) + f("count", x.n) + f("last", when(x.lastAt)) + "</div>"; }, "Nothing blocked in 24 h.");
  }
  out.push(card("Egress errors", "24 h", eb, E.egress));

  // spend
  var b = d.budget;
  var max = 0; (b && b.byDay || []).forEach(function(x){ if (x.usd > max) max = x.usd; });
  out.push(card("Spend per day", "14 days, Hong Kong days", rows(b && b.byDay, function(x){
    return '<div class="row"><span class="t">' + esc(x.day) + '</span><span class="mini" style="width:90px"><i style="width:' + (max ? (x.usd / max * 100).toFixed(0) : 0) + '%"></i></span><span class="main">' + usd(x.usd) + "</span>" + f("calls", x.calls) + "</div>";
  }, "No model calls in 14 days."), E.budget));
  out.push(card("Spend per person", "today · this month · since the budget started", rows(b && b.byUser, function(u){
    return '<div class="row"><span class="main">' + who(u.owner) + '</span><span class="r">' + usd(u.today) + " · " + usd(u.month) + " · " + usd(u.sinceBudget) + "</span></div>";
  }, "No model calls this month."), null));
  out.push(card("Spend per model", "today · this month · since the budget started", rows(b && b.byModel, function(m){
    return '<div class="row"><span class="main"><code>' + esc(m.model) + '</code></span><span class="r">' + usd(m.today) + " · " + usd(m.month) + " · " + usd(m.sinceBudget) + "</span>" + f("calls this month", m.callsMonth) + "</div>";
  }, "No model calls this month."), null));

  // people, each with View as (opens YYC AI as that person in a new tab; the router allows it for admins only)
  out.push(card("Conversations per staff member", "counted from sandboxes (first message); most recent first", rows(d.people, function(u){
    var va = u.former ? "" : '<form class="va" method="post" action="/api/jarvis/view-as" target="_blank"><input type="hidden" name="user" value="' + esc(u.owner) + '"><button type="submit" title="Open YYC AI as this person in a new tab (read-only; recorded below)">View as</button></form>';
    return '<div class="row"><span class="main">' + who(u.owner) + (u.former ? ' <span class="chip">not a user now</span>' : "") + "</span>" + f("total", u.total) + f("today", u.today) + f("7 days", u.last7d) + f("open now", u.live) + f("deleted", u.deleted) + f("last active", u.lastActiveAt ? when(u.lastActiveAt) : "never") + va + "</div>";
  }, "No users."), E.people));

  // view as: each view and everything sent as someone (staff are not told)
  var VA = {start: "started viewing", message: "sent a message as", conversation: "started a conversation as"};
  out.push(card("View as", "admins viewing YYC AI as someone, and what they sent as them; latest 100", rows(d.viewAs, function(x){
    return '<div class="row">' + t(x.at) + '<span class="main' + (x.event === "start" ? "" : " warmc") + '">' + who(x.admin) + " " + esc(VA[x.event] || x.event) + " " + who(x.user) + "</span>" +
      (x.conversationId ? f("conversation", "<code>" + short(x.conversationId) + "</code>") : "") + (x.chars != null ? f("length", x.chars + " chars") : "") + (x.session ? f("view", "<code>" + short(x.session) + "</code>") : "") + "</div>";
  }, "Nobody has viewed as anyone."), E["view as"]));
  return out.join("");
}

function render(d){
  var H = d.health || {state: "ok", findings: []};
  var v = document.getElementById("verdict");
  v.className = "pill s-" + H.state;
  v.textContent = LABEL[H.state] + (H.findings.length ? " · " + H.findings.length : "");
  document.getElementById("now-body").innerHTML = renderNow(d);
  document.getElementById("history-body").innerHTML = renderHistory(d);
  document.getElementById("meta").textContent = "updated " + when(d.at) + " HKT · " + d.provider + " · image " + String(d.sandboxImage || "").split(":").pop().slice(0, 12);
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

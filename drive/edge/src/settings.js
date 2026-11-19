// What the page asks before it shows the folder: its settings, the sign-in it believes in, who is signed in, which
// drive there is and what the server offers. None of it depends on the folder, so it is answered here, at the edge,
// from the person Access signed in. The page's calls were recorded from OpenCloud 9.1.0 (YYC-DRIVE.md).

/** The one drive's id: storage id `$` space id. */
export const STORAGE = "00000000-0000-4000-8000-000000000001";
export const DRIVE = `${STORAGE}$${STORAGE}`;
/** The header that names the signed-in person to the server (its --user-header). */
export const USER_HEADER = "cf-access-authenticated-user-email";
/** tus pieces the page sends (bytes); the server takes pieces of up to this size. */
export const CHUNK = 10_000_000;
const YEAR = 31_536_000;

const json = (v, status = 200) =>
  new Response(JSON.stringify(v), { status, headers: { "Content-Type": "application/json", "Cache-Control": "no-store" } });
const ocs = (data) => ({ ocs: { meta: { status: "ok", statuscode: 100, message: "OK" }, data } });
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
const b64url = (s) => btoa(String.fromCharCode(...new TextEncoder().encode(s))).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");

export function config(base) {
  return {
    server: `${base}/`,
    theme: `${base}/themes/opencloud/theme.json`,
    openIdConnect: { metadata_url: `${base}/.well-known/openid-configuration`, authority: base, client_id: "web", response_type: "code", scope: "openid profile email" },
    apps: ["files", "search", "text-editor", "pdf-viewer", "preview"],
    options: { tokenStorageLocal: true, contextHelpersReadMore: false, disableFeedbackLink: true, disableSponsorLink: true, embed: {}, concurrentRequests: { shares: {} } },
  };
}

export function oidcMeta(base) {
  return {
    issuer: base,
    authorization_endpoint: `${base}/oidc/authorize`,
    token_endpoint: `${base}/oidc/token`,
    userinfo_endpoint: `${base}/oidc/userinfo`,
    end_session_endpoint: `${base}/oidc/logout`,
    jwks_uri: `${base}/oidc/jwks`,
    scopes_supported: ["openid", "profile", "email"],
    response_types_supported: ["code"],
    subject_types_supported: ["public"],
    id_token_signing_alg_values_supported: ["none"],
    code_challenge_methods_supported: ["S256"],
  };
}

/** What this drive can do. Everything the page would offer beyond one folder is off. */
export function capabilities() {
  const version = { major: 0, minor: 1, micro: 0, string: "0.1.0", edition: "", product: "yyc-drive", productversion: "0.1.0", channel: "" };
  return {
    capabilities: {
      core: {
        pollinterval: 60, "webdav-root": "remote.php/webdav", "check-for-updates": false, "support-url-signing": true, "support-sse": false, "support-radicale": false,
        status: { installed: true, maintenance: false, needsDbUpgrade: false, version: "0.1.0.0", versionstring: "0.1.0", edition: "", productname: "yyc-drive", product: "yyc-drive", productversion: "0.1.0" },
      },
      checksums: { supportedTypes: [], preferredUploadType: "" },
      files: {
        privateLinks: false, bigfilechunking: false, undelete: false, versioning: false, favorites: false, full_text_search: false, tags: false, blacklisted_files: [],
        tus_support: { version: "1.0.0", resumable: "1.0.0", extension: "creation,creation-with-upload", max_chunk_size: CHUNK, http_method_override: "" },
        archivers: [], app_providers: [],
      },
      dav: { chunking: "", reports: ["search-files"], chunkingParallelUploadDisabled: false },
      files_sharing: { api_enabled: false, group_sharing: false, sharing_roles: false, resharing: false, public: { enabled: false }, user: { send_mail: false, profile_picture: false }, federation: { outgoing: false, incoming: false }, search_min_length: 3 },
      spaces: { version: "1.0.0", enabled: true, projects: false, share_jail: false, max_quota: 0, default_quota: 0 },
      graph: {
        "personal-data-export": false,
        users: { read_only_attributes: ["user.onPremisesSamAccountName", "user.displayName", "user.mail", "user.passwordProfile", "user.appRoleAssignments", "user.memberOf", "user.accountEnabled", "drive.quota"], create_disabled: true, delete_disabled: true, change_password_self_disabled: true, edit_login_allowed_disabled: true },
      },
      search: { property: { name: { enabled: true } } },
      notifications: { "ocs-endpoints": [], configurable: false },
    },
    version,
  };
}

export function drive(name, base, user) {
  return {
    id: DRIVE, driveAlias: "personal/drive", driveType: "personal", name,
    owner: { user: { displayName: "", id: user } },
    quota: { remaining: Number.MAX_SAFE_INTEGER, state: "normal", total: 0, used: 0 },
    root: { id: DRIVE, eTag: '"root"', webDavUrl: `${base}/dav/spaces/${DRIVE}` },
    webUrl: `${base}/f/${DRIVE}`,
  };
}

/**
 * The answer to one of the page's settings or sign-in calls, or null when `path` is not one (the folder's calls go
 * to the server). `base`: the drive's address; `user`: the signed-in person's email; `name`: the drive's name.
 */
export function answer(method, path, search, { base, user, name, now = Math.floor(Date.now() / 1000) }) {
  const who = user.split("@")[0];
  const q = new URLSearchParams(search);
  const is = (m, ...paths) => method === m && paths.includes(path);
  if (is("GET", "/config.json")) return json(config(base));
  if (is("GET", "/.well-known/openid-configuration")) return json(oidcMeta(base));
  if (is("GET", "/oidc/jwks")) return json({ keys: [] });
  if (is("GET", "/oidc/authorize")) {
    // signed in already (Access): straight back to the page with a code it trades below
    const to = q.get("redirect_uri") || base;
    const loc = `${to}${to.includes("?") ? "&" : "?"}code=drive&state=${encodeURIComponent(q.get("state") || "")}`;
    return new Response(null, { status: 302, headers: { Location: loc, "Cache-Control": "no-store" } });
  }
  if (is("GET", "/oidc/logout")) return new Response(null, { status: 302, headers: { Location: `${base}/`, "Cache-Control": "no-store" } });
  if (is("POST", "/oidc/token")) {
    const claims = { iss: base, sub: user, aud: "web", iat: now, exp: now + YEAR, name: who, email: user, preferred_username: who };
    const idToken = `${b64url('{"alg":"none","typ":"JWT"}')}.${b64url(JSON.stringify(claims))}.`;
    return json({ access_token: "drive", token_type: "Bearer", expires_in: YEAR, id_token: idToken, scope: "openid profile email" });
  }
  if (path === "/oidc/userinfo") return json({ sub: user, name: who, email: user, preferred_username: who });
  if (is("GET", "/.well-known/webfinger"))
    return json({
      subject: `${base}/`,
      properties: { "http://opencloud.eu/ns/oidc/client_id": "web", "http://opencloud.eu/ns/oidc/scopes": ["openid", "profile", "email"] },
      links: [{ rel: "http://openid.net/specs/connect/1.0/issuer", href: base }],
    });
  if (is("GET", "/ocs/v1.php/cloud/capabilities", "/ocs/v2.php/cloud/capabilities")) return json(ocs(capabilities()));
  if (is("GET", "/ocs/v1.php/cloud/user/signing-key", "/ocs/v2.php/cloud/user/signing-key"))
    // The page reads this one answer as XML. It signs links with the key; Access's sign-in is what guards them.
    return new Response(
      `<?xml version="1.0"?><ocs><meta><status>ok</status><statuscode>100</statuscode><message>OK</message></meta><data><user>${esc(user)}</user><signing-key>drive</signing-key></data></ocs>`,
      { headers: { "Content-Type": "application/xml; charset=utf-8", "Cache-Control": "no-store" } });
  if (is("GET", "/ocs/v2.php/apps/notifications/api/v1/notifications")) return json(ocs([]));
  if (is("GET", "/graph/v1.0/me"))
    return json({ id: user, displayName: who, mail: user, onPremisesSamAccountName: who, preferredLanguage: "en", userType: "Member", identities: [{ issuer: base, issuerAssignedId: user }] });
  if (is("PATCH", "/graph/v1.0/me")) return json({ id: user, displayName: who, mail: user, onPremisesSamAccountName: who });
  if (is("GET", "/graph/v1.0/me/drives", "/graph/v1beta1/me/drives", "/graph/v1.0/drives", "/graph/v1beta1/drives")) {
    const projects = (q.get("$filter") || "").includes("driveType eq project");
    return json({ value: projects ? [] : [drive(name, base, user)] });
  }
  if (method === "GET" && (path.startsWith("/graph/v1.0/drives/") || path.startsWith("/graph/v1beta1/drives/"))) return json(drive(name, base, user));
  if (is("GET", "/graph/v1beta1/roleManagement/permissions/roleDefinitions")) return json([]);
  if (is("GET", "/graph/v1.0/extensions/org.libregraph/tags")) return json({ value: [] });
  if (is("POST", "/api/v0/settings/roles-list")) return json({ bundles: [] }, 201);
  if (is("POST", "/api/v0/settings/assignments-list")) return json({ assignments: [] }, 201);
  if (is("POST", "/api/v0/settings/permissions-list")) return json({ permissions: [] }, 201);
  if (is("POST", "/api/v0/settings/values-list")) return json({ values: [] }, 201);
  if (is("GET", "/app/list")) return json({ "mime-types": [] });
  return null;
}

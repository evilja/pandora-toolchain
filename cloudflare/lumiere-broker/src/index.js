const API_VERSION = "1";
// Provider API bases, including each provider's own path prefix. Lulu and Voe
// hang their endpoints under `/api`; Byse serves them at the root. These hosts
// rotate; when one moves, its old origin answers with a 301 that this Worker
// refuses to follow, so change it here.
const PROVIDER_API = {
  lulustream: "https://api.lulustream.com/api",
  voe: "https://voe.sx/api",
  byse: "https://api.byse.sx",
};
// Fallback player origins for the links Pandora publishes. These rotate
// independently of the API origins above — this is what broke DoodStream, whose
// doodstream.com and dood.li both moved to playmogo.com while its API kept
// answering — and a published link must name the live host, because the sites we
// hand it to store the URL and embed it long after the redirect chain changes.
// Byse reports its own current domain (see embedDomain), so its entry here is
// only the answer of last resort. Mirrors RemoteProvider::final_url on the
// Pandora side; both are checked by `test.mjs`.
const PROVIDER_EMBED = {
  lulustream: "https://luluvdo.com",
  voe: "https://voe.sx",
  byse: "https://byse.sx",
};
const OPERATION_TTL_SECONDS = 24 * 60 * 60;
const ACCESS_TOKEN_SKEW_MS = 60 * 1000;
const EMBED_DOMAIN_TTL_MS = 6 * 60 * 60 * 1000;
const tokenCache = new Map();
const embedDomainCache = new Map();

export default {
  async fetch(request, env) {
    try {
      if (new URL(request.url).pathname === "/health") {
        return json({ ok: true, version: API_VERSION });
      }
      await authorize(request, env);
      if (request.headers.get("X-Lumiere-Version") !== API_VERSION) {
        throw new ApiError(400, "version_mismatch", "Unsupported Lumiere protocol version");
      }
      const url = new URL(request.url);
      if (request.method === "GET" && url.pathname === "/v1/status") {
        return statusResponse(url, env);
      }
      if (request.method === "POST" && url.pathname === "/v1/drive/sessions") {
        return json(await startDriveSession(await readJson(request), env), 201);
      }
      if (request.method === "POST" && url.pathname === "/v1/drive/delete") {
        await deleteDriveFile(await readJson(request), env);
        return json({ deleted: true });
      }
      if (request.method === "POST" && url.pathname === "/v1/remote/start") {
        return json(await startRemote(await readJson(request), env), 202);
      }
      if (request.method === "POST" && url.pathname === "/v1/remote/status") {
        return json(await remoteStatus(await readJson(request), env));
      }
      throw new ApiError(404, "not_found", "Lumiere endpoint was not found");
    } catch (error) {
      if (error instanceof ApiError) {
        return json({ error: { code: error.code, message: error.message } }, error.status);
      }
      console.error("Lumiere internal error");
      return json(
        { error: { code: "internal_error", message: "Lumiere broker failed internally" } },
        500,
      );
    }
  },
};

async function authorize(request, env) {
  const expected = String(env.LUMIERE_CLIENT_TOKEN || "");
  const header = request.headers.get("Authorization") || "";
  const presented = header.startsWith("Bearer ") ? header.slice(7).trim() : "";
  if (!expected || !presented || !(await secureEqual(expected, presented))) {
    throw new ApiError(401, "unauthorized", "Invalid Lumiere client token");
  }
}

async function secureEqual(left, right) {
  const encoder = new TextEncoder();
  const [leftHash, rightHash] = await Promise.all([
    crypto.subtle.digest("SHA-256", encoder.encode(left)),
    crypto.subtle.digest("SHA-256", encoder.encode(right)),
  ]);
  const a = new Uint8Array(leftHash);
  const b = new Uint8Array(rightHash);
  let different = 0;
  for (let index = 0; index < a.length; index += 1) different |= a[index] ^ b[index];
  return different === 0;
}

function statusResponse(url, env) {
  const profiles = driveProfiles(env);
  const requested = url.searchParams.get("profile") || "";
  return json({
    providers: {
      global_drive: Boolean(profileFor(profiles, "global")),
      requested_drive: Boolean(requested && profileFor(profiles, requested)),
      lulustream: Boolean(env.LULUSTREAM_API_KEY),
      voe: Boolean(env.VOE_API_KEY),
      byse: Boolean(env.BYSE_API_KEY),
    },
  });
}

async function startDriveSession(body, env) {
  validateRequestId(body.request_id);
  const length = positiveInteger(body.content_length, "content_length");
  const contentType = safeContentType(body.content_type);
  if (!/^[a-f0-9]{32}$/i.test(String(body.expected_md5 || ""))) {
    throw new ApiError(400, "invalid_md5", "expected_md5 must be a hexadecimal MD5 digest");
  }
  if (!Array.isArray(body.candidates) || body.candidates.length === 0 || body.candidates.length > 8) {
    throw new ApiError(400, "invalid_candidates", "One to eight Drive candidates are required");
  }
  const profiles = driveProfiles(env);
  let selected = null;
  for (let index = 0; index < body.candidates.length; index += 1) {
    const candidate = validateDriveCandidate(body.candidates[index]);
    const profile = profileFor(profiles, candidate.profile);
    const rootId = profile?.roots?.[candidate.root];
    if (profile && validDriveId(rootId)) {
      selected = { index, candidate, profile, rootId };
      break;
    }
  }
  if (!selected) {
    throw new ApiError(409, "drive_profile_unavailable", "No requested Drive profile and root is configured");
  }

  const accessToken = await googleAccessToken(selected.candidate.profile, selected.profile);
  const parentId = await ensureFolderPath(
    accessToken,
    selected.rootId,
    selected.candidate.folder_path,
  );
  const fileId = await generateDriveFileId(accessToken);
  const deleteToken = randomCapability();
  const deleteTokenHash = await sha256(deleteToken);
  const endpoint = new URL("https://www.googleapis.com/upload/drive/v3/files");
  endpoint.searchParams.set("uploadType", "resumable");
  endpoint.searchParams.set("supportsAllDrives", "true");
  endpoint.searchParams.set("fields", "id,name,size,md5Checksum");
  const response = await fetch(endpoint, {
    method: "POST",
    redirect: "manual",
    headers: {
      Authorization: `Bearer ${accessToken}`,
      "Content-Type": "application/json; charset=UTF-8",
      "X-Upload-Content-Length": String(length),
      "X-Upload-Content-Type": contentType,
    },
    body: JSON.stringify({
      id: fileId,
      name: selected.candidate.filename,
      parents: [parentId],
      appProperties: { lumiereDelete: deleteTokenHash },
    }),
  });
  const location = response.headers.get("Location") || "";
  if (!response.ok || !validDriveSessionUrl(location)) {
    throw new ApiError(502, "drive_session_failed", "Google Drive did not create an upload session");
  }
  return {
    upload_url: location,
    candidate_index: selected.index,
    profile: selected.candidate.profile,
    root: selected.candidate.root,
    parent_id: parentId,
    file_id: fileId,
    delete_token: deleteToken,
  };
}

async function deleteDriveFile(body, env) {
  const fileId = String(body.file_id || "").trim();
  if (!validDriveId(fileId)) {
    throw new ApiError(400, "invalid_file_id", "Drive file id is invalid");
  }
  const profileId = safeIdentifier(body.profile, "profile");
  const deleteToken = String(body.delete_token || "").trim();
  if (!/^[A-Za-z0-9_-]{43}$/.test(deleteToken)) {
    throw new ApiError(400, "invalid_delete_token", "Drive deletion capability is invalid");
  }
  const profile = profileFor(driveProfiles(env), profileId);
  if (!profile) {
    throw new ApiError(409, "drive_profile_unavailable", "Requested Drive profile is not configured");
  }
  const accessToken = await googleAccessToken(profileId, profile);
  const metadataUrl = new URL(`https://www.googleapis.com/drive/v3/files/${encodeURIComponent(fileId)}`);
  metadataUrl.searchParams.set("supportsAllDrives", "true");
  metadataUrl.searchParams.set("fields", "id,appProperties");
  const metadata = await googleJson(metadataUrl, accessToken);
  const storedHash = String(metadata?.appProperties?.lumiereDelete || "");
  const presentedHash = await sha256(deleteToken);
  if (!storedHash || !(await secureEqual(storedHash, presentedHash))) {
    throw new ApiError(403, "delete_capability_rejected", "Drive file is not owned by this Lumiere capability");
  }
  const endpoint = new URL(`https://www.googleapis.com/drive/v3/files/${encodeURIComponent(fileId)}`);
  endpoint.searchParams.set("supportsAllDrives", "true");
  const response = await fetch(endpoint, {
    method: "DELETE",
    redirect: "manual",
    headers: { Authorization: `Bearer ${accessToken}` },
  });
  if (!response.ok && response.status !== 404) {
    throw new ApiError(502, "drive_delete_failed", "Google Drive rejected file deletion");
  }
}

async function startRemote(body, env) {
  const requestId = validateRequestId(body.request_id);
  const provider = remoteProvider(body.provider);
  const sourceUrl = validateSourceUrl(body.source_url, env);
  const filename = safeFilename(body.filename);
  const fingerprint = await sha256(`${provider}\n${sourceUrl}\n${filename}`);
  const operationKey = `remote:${provider}:${requestId}`;
  if (env.OPERATIONS) {
    const cached = await env.OPERATIONS.get(operationKey, "json");
    if (cached) {
      if (cached.fingerprint !== fingerprint) {
        throw new ApiError(409, "idempotency_conflict", "request_id was already used for a different upload");
      }
      return cached.operation;
    }
  }

  let operation;
  if (provider === "byse") operation = await startByse(sourceUrl, env);
  else if (provider === "lulustream") operation = await startLulustream(sourceUrl, env);
  else operation = await startVoe(sourceUrl, env);

  if (env.OPERATIONS) {
    try {
      await env.OPERATIONS.put(
        operationKey,
        JSON.stringify({ fingerprint, operation }),
        { expirationTtl: OPERATION_TTL_SECONDS },
      );
    } catch (_) {
      console.error("Lumiere idempotency write failed");
    }
  }
  return operation;
}

async function remoteStatus(body, env) {
  const operation = body.operation || {};
  const provider = remoteProvider(operation.provider);
  const operationId = safeOperationId(operation.operation_id);
  const fileCode = safeFileCode(operation.file_code);
  // Pandora sets source_drained once the provider has pulled every byte from the
  // capability URL. Providers whose queue entry stops reporting (or reports a
  // word this Worker does not know) otherwise stay "uploading" forever, so that
  // is the point at which asking file/info is both cheap and decisive.
  const sourceDrained = body.source_drained === true;
  let status;
  if (provider === "byse") status = await byseStatus(operationId, fileCode, env);
  else if (provider === "lulustream") status = await lulustreamStatus(operationId, fileCode, env);
  else status = await voeStatus(operationId, fileCode, env);

  if (sourceDrained && (status.state === "queued" || status.state === "uploading")) {
    const playable = await fileInfoPlayable(provider, fileCode, providerKey(provider, env));
    if (playable) {
      return {
        ...status,
        state: "complete",
        url: await finalUrl(provider, fileCode, env),
        detail: joinDetail(status.detail, "file/info reports the file is playable"),
      };
    }
    return { ...status, detail: joinDetail(status.detail, "file/info not playable yet") };
  }
  return status;
}

function providerKey(provider, env) {
  if (provider === "byse") {
    return requiredSecret(env.BYSE_API_KEY, "byse_not_configured", "Byse is not configured");
  }
  if (provider === "lulustream") {
    return requiredSecret(env.LULUSTREAM_API_KEY, "lulustream_not_configured", "LuluStream is not configured");
  }
  return requiredSecret(env.VOE_API_KEY, "voe_not_configured", "Voe is not configured");
}

async function startByse(sourceUrl, env) {
  const key = requiredSecret(env.BYSE_API_KEY, "byse_not_configured", "Byse is not configured");
  const endpoint = new URL(`${PROVIDER_API.byse}/remote/add`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("url", sourceUrl);
  const data = await providerJson(endpoint, "Byse");
  const fileCode = providerFileCode(data?.result?.filecode, "Byse", data);
  return { provider: "byse", operation_id: fileCode, file_code: fileCode };
}

async function startLulustream(sourceUrl, env) {
  const key = requiredSecret(env.LULUSTREAM_API_KEY, "lulustream_not_configured", "LuluStream is not configured");
  const endpoint = new URL(`${PROVIDER_API.lulustream}/upload/url`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("url", sourceUrl);
  const data = await providerJson(endpoint, "LuluStream");
  const fileCode = providerFileCode(data?.result?.filecode, "LuluStream", data);
  return { provider: "lulustream", operation_id: fileCode, file_code: fileCode };
}

async function startVoe(sourceUrl, env) {
  const key = requiredSecret(env.VOE_API_KEY, "voe_not_configured", "Voe is not configured");
  const endpoint = new URL(`${PROVIDER_API.voe}/upload/url`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("url", sourceUrl);
  const data = await providerJson(endpoint, "Voe");
  const fileCode = providerFileCode(data?.result?.file_code, "Voe", data);
  const operationId = safeOperationId(data?.result?.queueID ?? fileCode);
  return { provider: "voe", operation_id: operationId, file_code: fileCode };
}

async function byseStatus(_operationId, fileCode, env) {
  const key = requiredSecret(env.BYSE_API_KEY, "byse_not_configured", "Byse is not configured");
  const endpoint = new URL(`${PROVIDER_API.byse}/remote/status`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("file_code", fileCode);
  const data = await providerJson(endpoint, "Byse");
  // Byse answers about the file code it was asked for, so a single object is the
  // normal shape here; matchFileCode still guards the case where it returns a
  // list, and "not listed" stays not listed rather than matching a stranger.
  const item = matchFileCode(data.result, fileCode);
  if (!item) {
    return { ...(await fileInfoFallback("byse", fileCode, key, env)), detail: "remote/status listed no entry" };
  }
  // A populated error_msg is Byse's failure channel; its status word can still
  // read WORKING alongside one.
  const failed = String(item.error_msg || "").trim() !== "";
  const state = failed ? "failed" : normalizeTextState(item.status);
  return {
    state,
    progress: numeric(String(item.progress ?? "").replace("%", "")),
    url: state === "complete" ? await finalUrl("byse", fileCode, env) : undefined,
    embed_domain: await embedDomain("byse", env),
    detail: describeItem(item, ["status", "progress", "error_msg"]),
  };
}

// Providers spell the code `filecode` when they start an upload and `file_code`
// in their queue listing, so accept either rather than silently matching nothing.
function itemFileCode(item) {
  return String(item?.file_code ?? item?.filecode ?? "").trim();
}

// A single-object result is the provider answering about the file we asked for;
// a list has to be searched, and no match means our transfer is genuinely absent.
function matchFileCode(result, fileCode) {
  if (!result) return undefined;
  if (!Array.isArray(result)) return result;
  return result.find((entry) => itemFileCode(entry) === fileCode);
}

async function lulustreamStatus(_operationId, fileCode, env) {
  const key = requiredSecret(env.LULUSTREAM_API_KEY, "lulustream_not_configured", "LuluStream is not configured");
  const endpoint = new URL(`${PROVIDER_API.lulustream}/file/url_uploads`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("file_code", fileCode);
  const data = await providerJson(endpoint, "LuluStream");
  const items = Array.isArray(data.result) ? data.result : [];
  const item = items.find((entry) => String(entry.file_code || "") === fileCode) || items[0];
  if (!item) {
    return { ...(await fileInfoFallback("lulustream", fileCode, key, env)), detail: "url_uploads listed no entry" };
  }
  const state = normalizeTextState(item.status);
  return {
    state,
    progress: numeric(item.progress),
    url: state === "complete" ? await finalUrl("lulustream", fileCode, env) : undefined,
    detail: describeItem(item, ["status", "progress", "error", "message"]),
  };
}

async function voeStatus(operationId, fileCode, env) {
  const key = requiredSecret(env.VOE_API_KEY, "voe_not_configured", "Voe is not configured");
  const endpoint = new URL(`${PROVIDER_API.voe}/upload/url/list`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("id", operationId);
  endpoint.searchParams.set("limit", "1");
  const data = await providerJson(endpoint, "Voe");
  const items = data?.list?.data || data?.result?.data || data?.result || [];
  const item = Array.isArray(items) ? items[0] : items;
  if (!item) {
    return { ...(await fileInfoFallback("voe", fileCode, key, env)), detail: "upload/url/list returned no entry" };
  }
  const code = Number(item.status);
  const state = code === 3 ? "complete" : code === 4 ? "failed" : "uploading";
  return {
    state,
    progress: numeric(item.percent),
    bytes_done: numeric(item.loaded_size),
    bytes_total: numeric(item.total_size),
    url: state === "complete" ? await finalUrl("voe", fileCode, env) : undefined,
    detail: describeItem(item, ["status", "percent", "loaded_size", "total_size", "message"]),
  };
}

async function fileInfoFallback(provider, fileCode, key, env) {
  const playable = await fileInfoPlayable(provider, fileCode, key);
  if (playable) return { state: "complete", url: await finalUrl(provider, fileCode, env) };
  return { state: "uploading" };
}

// The provider's own file record is the only signal that survives a queue entry
// disappearing or reporting a state this Worker does not recognise.
async function fileInfoPlayable(provider, fileCode, key) {
  const endpoint = new URL(`${PROVIDER_API[provider]}/file/info`);
  endpoint.searchParams.set("key", key);
  endpoint.searchParams.set("file_code", fileCode);
  try {
    const data = await providerJson(endpoint, provider);
    const result = Array.isArray(data.result) ? data.result[0] : data.result;
    return Boolean(
      result && Number(result.status) === 200
        && (result.canplay === undefined || Number(result.canplay) === 1),
    );
  } catch (_) {
    return false;
  }
}

// Echoes the fields the provider actually sent so an unmapped state shows up in
// Pandora's log as the provider's own word instead of a silent "uploading".
function describeItem(item, fields) {
  const parts = [];
  for (const field of fields) {
    const value = item?.[field];
    if (value === undefined || value === null || value === "") continue;
    parts.push(`${field}=${safeDetail(value)}`);
  }
  return parts.length > 0 ? parts.join(" ") : "provider sent no recognisable fields";
}

// Hostname only: a provider Location carries our API key in its query string.
function hostOf(raw) {
  try {
    return new URL(String(raw || "")).host;
  } catch (_) {
    return "";
  }
}

function joinDetail(existing, addition) {
  return existing ? `${existing}; ${addition}` : addition;
}

// Provider payloads can echo the source URL back at us, and that URL carries the
// capability token; strip anything URL-shaped before it can reach a log line.
function safeDetail(raw) {
  const value = String(raw)
    .replace(/[a-z][a-z0-9+.-]*:\/\/\S*/gi, "<url>")
    .replace(/[\r\n\t]+/g, " ")
    .trim();
  return value.length > 120 ? `${value.slice(0, 120)}…` : value;
}

async function providerJson(url, provider) {
  let response;
  try {
    response = await fetch(url, { redirect: "manual" });
  } catch (_) {
    throw new ApiError(502, "provider_unavailable", `${provider} is unavailable`);
  }
  // A moved API answers with a redirect this Worker deliberately does not follow.
  // Name the new host — and only the host, because the Location echoes our API
  // key in its query — so the next domain rotation diagnoses itself.
  if (response.status >= 300 && response.status < 400) {
    const moved = hostOf(response.headers.get("Location"));
    throw new ApiError(
      502,
      "provider_moved",
      moved
        ? `${provider} redirected its API to ${moved}; update PROVIDER_API`
        : `${provider} redirected its API without a target`,
    );
  }
  let data;
  try {
    data = await response.json();
  } catch (_) {
    throw new ApiError(502, "provider_protocol", `${provider} returned an invalid response`);
  }
  const envelopeStatus = Number(data?.status ?? response.status);
  if (!response.ok || (Number.isFinite(envelopeStatus) && envelopeStatus >= 400)) {
    const reason = safeDetail(data?.msg ?? data?.message ?? data?.error ?? "");
    throw new ApiError(
      502,
      "provider_rejected",
      reason
        ? `${provider} rejected the request (${envelopeStatus}): ${reason}`
        : `${provider} rejected the request (${envelopeStatus})`,
    );
  }
  return data;
}

async function googleAccessToken(profileId, profile) {
  validateDriveProfile(profile);
  const cached = tokenCache.get(profileId);
  if (cached && cached.expiresAt - ACCESS_TOKEN_SKEW_MS > Date.now()) return cached.token;
  const tokenUrl = new URL(profile.token_url || "https://oauth2.googleapis.com/token");
  if (
    tokenUrl.protocol !== "https:" ||
    !["oauth2.googleapis.com", "accounts.google.com"].includes(tokenUrl.hostname) ||
    (tokenUrl.port && tokenUrl.port !== "443") ||
    tokenUrl.username ||
    tokenUrl.password ||
    tokenUrl.search ||
    tokenUrl.hash
  ) {
    throw new ApiError(500, "drive_profile_invalid", "Drive token URL is not trusted");
  }
  const form = new URLSearchParams({
    client_id: profile.client_id,
    client_secret: profile.client_secret,
    refresh_token: profile.refresh_token,
    grant_type: "refresh_token",
  });
  const response = await fetch(tokenUrl, {
    method: "POST",
    redirect: "manual",
    headers: { "Content-Type": "application/x-www-form-urlencoded" },
    body: form,
  });
  let data = {};
  try { data = await response.json(); } catch (_) {}
  if (!response.ok || typeof data.access_token !== "string" || !data.access_token) {
    throw new ApiError(502, "drive_token_failed", "Google rejected the Drive profile credentials");
  }
  const expiresIn = Number(data.expires_in || 3600);
  tokenCache.set(profileId, {
    token: data.access_token,
    expiresAt: Date.now() + Math.max(60, expiresIn) * 1000,
  });
  return data.access_token;
}

async function generateDriveFileId(accessToken) {
  const endpoint = new URL("https://www.googleapis.com/drive/v3/files/generateIds");
  endpoint.searchParams.set("count", "1");
  endpoint.searchParams.set("space", "drive");
  endpoint.searchParams.set("type", "files");
  const response = await googleJson(endpoint, accessToken);
  const fileId = String(response?.ids?.[0] || "");
  if (!validDriveId(fileId)) {
    throw new ApiError(502, "drive_id_failed", "Google Drive did not reserve a file id");
  }
  return fileId;
}

async function ensureFolderPath(accessToken, rootId, folderPath) {
  let parentId = rootId;
  for (const component of folderComponents(folderPath)) {
    parentId = await ensureFolder(accessToken, parentId, component);
  }
  return parentId;
}

async function ensureFolder(accessToken, parentId, name) {
  const query = [
    "mimeType='application/vnd.google-apps.folder'",
    "trashed=false",
    `name='${driveQueryEscape(name)}'`,
    `'${driveQueryEscape(parentId)}' in parents`,
  ].join(" and ");
  const search = new URL("https://www.googleapis.com/drive/v3/files");
  search.searchParams.set("q", query);
  search.searchParams.set("fields", "files(id,name)");
  search.searchParams.set("pageSize", "1");
  search.searchParams.set("supportsAllDrives", "true");
  search.searchParams.set("includeItemsFromAllDrives", "true");
  const found = await googleJson(search, accessToken);
  const existing = found?.files?.[0]?.id;
  if (validDriveId(existing)) return existing;

  const create = new URL("https://www.googleapis.com/drive/v3/files");
  create.searchParams.set("supportsAllDrives", "true");
  create.searchParams.set("fields", "id");
  const created = await googleJson(create, accessToken, {
    method: "POST",
    body: JSON.stringify({
      name,
      mimeType: "application/vnd.google-apps.folder",
      parents: [parentId],
    }),
  });
  if (!validDriveId(created?.id)) {
    throw new ApiError(502, "drive_folder_failed", "Google Drive did not return a folder id");
  }
  return created.id;
}

async function googleJson(url, accessToken, options = {}) {
  const response = await fetch(url, {
    ...options,
    redirect: "manual",
    headers: {
      Authorization: `Bearer ${accessToken}`,
      ...(options.body ? { "Content-Type": "application/json" } : {}),
      ...(options.headers || {}),
    },
  });
  if (!response.ok) {
    throw new ApiError(502, "drive_request_failed", "Google Drive rejected a broker operation");
  }
  try {
    return await response.json();
  } catch (_) {
    throw new ApiError(502, "drive_protocol", "Google Drive returned invalid JSON");
  }
}

function driveProfiles(env) {
  if (!env.LUMIERE_DRIVE_PROFILES) return {};
  let profiles;
  try {
    profiles = JSON.parse(env.LUMIERE_DRIVE_PROFILES);
  } catch (_) {
    throw new ApiError(500, "drive_profiles_invalid", "LUMIERE_DRIVE_PROFILES is invalid JSON");
  }
  if (!profiles || typeof profiles !== "object" || Array.isArray(profiles)) {
    throw new ApiError(500, "drive_profiles_invalid", "LUMIERE_DRIVE_PROFILES must be an object");
  }
  return profiles;
}

function profileFor(profiles, profileId) {
  return Object.prototype.hasOwnProperty.call(profiles, profileId) ? profiles[profileId] : null;
}

function validateDriveProfile(profile) {
  for (const field of ["client_id", "client_secret", "refresh_token"]) {
    if (typeof profile?.[field] !== "string" || !profile[field].trim()) {
      throw new ApiError(500, "drive_profile_invalid", `Drive profile is missing ${field}`);
    }
  }
  if (!profile.roots || typeof profile.roots !== "object") {
    throw new ApiError(500, "drive_profile_invalid", "Drive profile roots are missing");
  }
}

function validateDriveCandidate(raw) {
  const candidate = {
    profile: safeIdentifier(raw?.profile, "profile"),
    root: safeIdentifier(raw?.root, "root"),
    folder_path: String(raw?.folder_path || "").trim(),
    filename: safeFilename(raw?.filename),
  };
  folderComponents(candidate.folder_path);
  return candidate;
}

function folderComponents(path) {
  if (typeof path !== "string" || path.length > 1000) {
    throw new ApiError(400, "invalid_folder_path", "Drive folder path is too long");
  }
  const parts = path.split("/").map((part) => part.trim()).filter(Boolean);
  if (parts.length > 20) {
    throw new ApiError(400, "invalid_folder_path", "Drive folder path is too deep");
  }
  for (const part of parts) {
    if (part === "." || part === ".." || part.length > 100 || /[\u0000-\u001f\u007f]/.test(part)) {
      throw new ApiError(400, "invalid_folder_path", "Drive folder path is invalid");
    }
  }
  return parts;
}

function driveQueryEscape(value) {
  return String(value).replaceAll("\\", "\\\\").replaceAll("'", "\\'");
}

function validateSourceUrl(raw, env) {
  let source;
  let allowed;
  try {
    source = new URL(String(raw || ""));
    allowed = new URL(String(env.LUMIERE_SOURCE_ORIGIN || ""));
  } catch (_) {
    throw new ApiError(400, "invalid_source_url", "Remote upload source URL is invalid");
  }
  if (
    source.protocol !== "https:" ||
    source.origin !== allowed.origin ||
    source.username || source.password || source.hash ||
    !source.pathname.startsWith("/lumiere/v1/files/")
  ) {
    throw new ApiError(400, "invalid_source_url", "Remote upload source is not an allowed Pandora capability");
  }
  return source.toString();
}

function validDriveSessionUrl(raw) {
  try {
    const url = new URL(raw);
    return url.protocol === "https:"
      && url.hostname === "www.googleapis.com"
      && (!url.port || url.port === "443")
      && !url.username
      && !url.password
      && !url.hash
      && url.pathname === "/upload/drive/v3/files"
      && url.searchParams.get("uploadType") === "resumable"
      && Boolean(url.searchParams.get("upload_id"));
  } catch (_) {
    return false;
  }
}

function normalizeTextState(raw) {
  const state = String(raw || "").trim().toLowerCase();
  if (["complete", "completed", "done", "finished", "success", "ready"].includes(state)) return "complete";
  if (["error", "failed", "failure", "cancelled", "canceled"].includes(state)) return "failed";
  if (["queued", "pending", "waiting", "new"].includes(state)) return "queued";
  return "uploading";
}

async function finalUrl(provider, fileCode, env) {
  return `https://${await embedDomain(provider, env)}/e/${fileCode}`;
}

// Byse is the only provider that will tell us where its player currently lives,
// so it is the only one whose published domain survives a rotation without a
// redeploy. The answer is cached per isolate and any failure falls back to the
// static entry rather than publishing nothing.
async function embedDomain(provider, env) {
  const fallback = hostOf(PROVIDER_EMBED[provider]);
  if (provider !== "byse") return fallback;
  const cached = embedDomainCache.get(provider);
  if (cached && cached.expiresAt > Date.now()) return cached.domain;
  let domain = fallback;
  try {
    const endpoint = new URL(`${PROVIDER_API.byse}/get/domain`);
    endpoint.searchParams.set("key", providerKey("byse", env));
    const data = await providerJson(endpoint, "Byse");
    domain = safeEmbedDomain(data?.new_domain) || fallback;
  } catch (_) {
    console.error("Lumiere embed domain lookup failed; using the configured fallback");
  }
  embedDomainCache.set(provider, { domain, expiresAt: Date.now() + EMBED_DOMAIN_TTL_MS });
  return domain;
}

// The provider supplies this, so it is treated as untrusted input: a bare
// hostname only, never a scheme, port, credentials, or path that could redirect
// a published link somewhere other than the host it names.
function safeEmbedDomain(raw) {
  const value = String(raw || "").trim().toLowerCase();
  if (!/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$/.test(value)) return "";
  if (value.length > 100) return "";
  return value;
}

function remoteProvider(raw) {
  const value = String(raw || "");
  if (!["lulustream", "voe", "byse"].includes(value)) {
    throw new ApiError(400, "invalid_provider", "Unsupported remote upload provider");
  }
  return value;
}

function validateRequestId(raw) {
  const value = String(raw || "").trim();
  if (!/^[A-Za-z0-9:._-]{1,160}$/.test(value)) {
    throw new ApiError(400, "invalid_request_id", "request_id is invalid");
  }
  return value;
}

function safeIdentifier(raw, field) {
  const value = String(raw || "").trim();
  if (!/^[A-Za-z0-9:_-]{1,100}$/.test(value)) {
    throw new ApiError(400, `invalid_${field}`, `${field} is invalid`);
  }
  return value;
}

function safeFilename(raw) {
  const value = String(raw || "").trim();
  if (!value || value.length > 180 || /[\\/\u0000-\u001f\u007f]/.test(value) || value === "." || value === "..") {
    throw new ApiError(400, "invalid_filename", "filename is invalid");
  }
  return value;
}

function safeOperationId(raw) {
  const value = String(raw ?? "").trim();
  if (!/^[A-Za-z0-9_-]{1,100}$/.test(value)) {
    throw new ApiError(400, "invalid_operation", "Remote operation id is invalid");
  }
  return value;
}

// A start call that returns no file code is the provider declining the job; its
// own message is the only thing that explains why.
function providerFileCode(raw, provider, data) {
  const value = String(raw ?? "").trim();
  if (!/^[A-Za-z0-9_-]{1,100}$/.test(value)) {
    const reason = safeDetail(data?.msg ?? data?.message ?? data?.error ?? "");
    throw new ApiError(
      502,
      "provider_protocol",
      reason ? `${provider} returned no file code: ${reason}` : `${provider} returned no file code`,
    );
  }
  return value;
}

function safeFileCode(raw) {
  const value = String(raw ?? "").trim();
  if (!/^[A-Za-z0-9_-]{1,100}$/.test(value)) {
    throw new ApiError(502, "provider_protocol", "Provider returned an invalid file code");
  }
  return value;
}

function safeContentType(raw) {
  const value = String(raw || "").trim();
  if (!value || value.length > 100 || /[\r\n]/.test(value)) {
    throw new ApiError(400, "invalid_content_type", "content_type is invalid");
  }
  return value;
}

function positiveInteger(raw, field) {
  const value = Number(raw);
  if (!Number.isSafeInteger(value) || value <= 0) {
    throw new ApiError(400, `invalid_${field}`, `${field} must be a positive integer`);
  }
  return value;
}

function validDriveId(raw) {
  return typeof raw === "string" && /^[A-Za-z0-9_-]{1,200}$/.test(raw);
}

function requiredSecret(raw, code, message) {
  const value = String(raw || "").trim();
  if (!value) throw new ApiError(409, code, message);
  return value;
}

function numeric(raw) {
  const value = Number(raw);
  return Number.isFinite(value) && value >= 0 ? value : undefined;
}

async function sha256(value) {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(value));
  return [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, "0")).join("");
}

function randomCapability() {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

async function readJson(request) {
  const declared = Number(request.headers.get("Content-Length") || 0);
  if (declared > 64 * 1024) throw new ApiError(413, "body_too_large", "Request body is too large");
  const text = await request.text();
  if (new TextEncoder().encode(text).byteLength > 64 * 1024) {
    throw new ApiError(413, "body_too_large", "Request body is too large");
  }
  let body;
  try {
    body = JSON.parse(text);
  } catch (_) {
    throw new ApiError(400, "invalid_json", "Request body must be JSON");
  }
  if (!body || typeof body !== "object" || Array.isArray(body)) {
    throw new ApiError(400, "invalid_json", "Request body must be a JSON object");
  }
  return body;
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      "Content-Type": "application/json; charset=utf-8",
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
    },
  });
}

class ApiError extends Error {
  constructor(status, code, message) {
    super(message);
    this.status = status;
    this.code = code;
  }
}

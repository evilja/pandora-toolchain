import assert from "node:assert/strict";
import worker from "./src/index.js";

const env = {
  LUMIERE_CLIENT_TOKEN: "test-token",
  LUMIERE_SOURCE_ORIGIN: "https://files.example.com",
  LUMIERE_DRIVE_PROFILES: JSON.stringify({
    global: {
      client_id: "id",
      client_secret: "secret",
      refresh_token: "refresh",
      roots: { default: "root" },
    },
  }),
};

const health = await worker.fetch(new Request("https://broker.example/health"), env);
assert.equal(health.status, 200);
assert.deepEqual(await health.json(), { ok: true, version: "1" });

const unauthorized = await worker.fetch(new Request("https://broker.example/v1/status"), env);
assert.equal(unauthorized.status, 401);

const headers = {
  Authorization: "Bearer test-token",
  "X-Lumiere-Version": "1",
};
const status = await worker.fetch(
  new Request("https://broker.example/v1/status?profile=guild%3A1", { headers }),
  env,
);
assert.equal(status.status, 200);
assert.deepEqual((await status.json()).providers, {
  global_drive: true,
  requested_drive: false,
  doodstream: false,
  lulustream: false,
  voe: false,
  abyss: false,
});

const invalidSource = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:1",
      provider: "doodstream",
      source_url: "https://attacker.example/file.mp4",
      filename: "file.mp4",
    }),
  }),
  env,
);
assert.equal(invalidSource.status, 400);
assert.equal((await invalidSource.json()).error.code, "invalid_source_url");

const realFetch = globalThis.fetch;
let deleteTokenHash = "";
let uploadMetadata = null;
globalThis.fetch = async (input, init = {}) => {
  const url = new URL(input instanceof Request ? input.url : input);
  const method = init.method || (input instanceof Request ? input.method : "GET");
  if (url.hostname === "oauth2.googleapis.com") {
    return Response.json({ access_token: "access", expires_in: 3600 });
  }
  if (url.pathname === "/drive/v3/files/generateIds" && method === "GET") {
    return Response.json({ ids: ["file-id"] });
  }
  if (url.pathname === "/drive/v3/files" && method === "GET") {
    return Response.json({ files: [{ id: "folder-id" }] });
  }
  if (url.pathname === "/upload/drive/v3/files" && method === "POST") {
    uploadMetadata = JSON.parse(init.body);
    deleteTokenHash = uploadMetadata.appProperties.lumiereDelete;
    return new Response(null, {
      status: 200,
      headers: {
        Location: "https://www.googleapis.com/upload/drive/v3/files?uploadType=resumable&upload_id=test",
      },
    });
  }
  if (url.pathname === "/drive/v3/files/file-id" && method === "GET") {
    return Response.json({ id: "file-id", appProperties: { lumiereDelete: deleteTokenHash } });
  }
  if (url.pathname === "/drive/v3/files/file-id" && method === "DELETE") {
    return new Response(null, { status: 204 });
  }
  if (url.hostname === "doodapi.co" && url.pathname === "/api/upload/url") {
    assert.equal(url.searchParams.get("key"), "dood-key");
    assert.equal(
      url.searchParams.get("url"),
      "https://files.example.com/lumiere/v1/files/abc/test.mp4",
    );
    return Response.json({ status: 200, result: { filecode: "dood-file" } });
  }
  if (url.hostname === "doodapi.co" && url.pathname === "/api/urlupload/status") {
    return Response.json({
      status: 200,
      result: [{
        status: "finished",
        file_code: "dood-file",
        bytes_downloaded: "10",
        bytes_total: "10",
      }],
    });
  }
  throw new Error(`unexpected test fetch ${method} ${url}`);
};

const driveSession = await worker.fetch(
  new Request("https://broker.example/v1/drive/sessions", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:drive:1",
      candidates: [{
        profile: "global",
        root: "default",
        folder_path: "test",
        filename: "test.mp4",
      }],
      content_length: 10,
      content_type: "video/mp4",
      expected_md5: "d41d8cd98f00b204e9800998ecf8427e",
    }),
  }),
  env,
);
assert.equal(driveSession.status, 201);
const session = await driveSession.json();
assert.equal(session.file_id, "file-id");
assert.equal(uploadMetadata.id, "file-id");
assert.match(session.delete_token, /^[A-Za-z0-9_-]{43}$/);
assert.match(deleteTokenHash, /^[a-f0-9]{64}$/);

const driveDelete = await worker.fetch(
  new Request("https://broker.example/v1/drive/delete", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      profile: "global",
      file_id: "file-id",
      delete_token: session.delete_token,
    }),
  }),
  env,
);
assert.equal(driveDelete.status, 200);

const remoteEnv = { ...env, DOODSTREAM_API_KEY: "dood-key" };
const remoteStart = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:remote:1",
      provider: "doodstream",
      source_url: "https://files.example.com/lumiere/v1/files/abc/test.mp4",
      filename: "test.mp4",
    }),
  }),
  remoteEnv,
);
assert.equal(remoteStart.status, 202);
const operation = await remoteStart.json();
assert.equal(operation.file_code, "dood-file");

const remoteStatus = await worker.fetch(
  new Request("https://broker.example/v1/remote/status", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({ operation }),
  }),
  remoteEnv,
);
assert.equal(remoteStatus.status, 200);
assert.equal((await remoteStatus.json()).state, "complete");
globalThis.fetch = realFetch;

console.log("Lumiere Worker contract tests passed");

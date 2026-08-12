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
  lulustream: false,
  voe: false,
  byse: false,
});

const invalidSource = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:1",
      provider: "byse",
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
let byseStatusResult = { status: "FINISHED", progress: "100%", error_msg: "" };
let bysePlayable = false;
let byseDomain = "byse.sx";
let luluStartResponse = { status: 200, result: { filecode: "lulu-file" } };
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
  if (url.hostname === "api.byse.sx" && url.pathname === "/remote/add") {
    assert.equal(url.searchParams.get("key"), "byse-key");
    assert.equal(
      url.searchParams.get("url"),
      "https://files.example.com/lumiere/v1/files/abc/test.mp4",
    );
    return Response.json({ status: 200, result: { filecode: "byse-file" } });
  }
  if (url.hostname === "api.byse.sx" && url.pathname === "/remote/status") {
    return Response.json({ status: 200, result: byseStatusResult });
  }
  if (url.hostname === "api.byse.sx" && url.pathname === "/get/domain") {
    return Response.json({ status: 200, old_domain: "filemoon.sx", new_domain: byseDomain });
  }
  if (url.hostname === "api.byse.sx" && url.pathname === "/file/info") {
    assert.equal(url.searchParams.get("file_code"), "byse-file");
    return Response.json({
      status: 200,
      result: [{ status: bysePlayable ? 200 : 404, canplay: bysePlayable ? 1 : 0 }],
    });
  }
  if (url.hostname === "api.lulustream.com" && url.pathname === "/api/upload/url") {
    if (luluStartResponse === "moved") {
      return new Response("<html>moved</html>", {
        status: 301,
        headers: { Location: `https://api.example.net/api/upload/url?key=lulu-key&url=${url.searchParams.get("url")}` },
      });
    }
    return Response.json(luluStartResponse);
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

const remoteEnv = { ...env, BYSE_API_KEY: "byse-key" };
const remoteStart = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:remote:1",
      provider: "byse",
      source_url: "https://files.example.com/lumiere/v1/files/abc/test.mp4",
      filename: "test.mp4",
    }),
  }),
  remoteEnv,
);
assert.equal(remoteStart.status, 202);
const operation = await remoteStart.json();
assert.equal(operation.file_code, "byse-file");

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

const statusRequest = (body, statusEnv = remoteEnv) =>
  worker.fetch(
    new Request("https://broker.example/v1/remote/status", {
      method: "POST",
      headers: { ...headers, "Content-Type": "application/json" },
      body: JSON.stringify(body),
    }),
    statusEnv,
  );

// A provider state this Worker does not recognise must reach Pandora as the
// provider's own word rather than a silent, permanent "uploading".
byseStatusResult = { status: "WORKING", progress: "55%", error_msg: "" };
const unmapped = await statusRequest({ operation });
assert.equal(unmapped.status, 200);
const unmappedBody = await unmapped.json();
assert.equal(unmappedBody.state, "uploading");
assert.match(unmappedBody.detail, /status=WORKING/);

// Once Pandora has served every byte, a playable file ends the poll instead of
// hanging until the transfer capability expires.
bysePlayable = true;
const drained = await statusRequest({ operation, source_drained: true });
const drainedBody = await drained.json();
assert.equal(drainedBody.state, "complete");
assert.equal(drainedBody.url, "https://byse.sx/e/byse-file");
assert.match(drainedBody.detail, /file\/info reports the file is playable/);

bysePlayable = false;
const stillEncoding = await statusRequest({ operation, source_drained: true });
const stillEncodingBody = await stillEncoding.json();
assert.equal(stillEncodingBody.state, "uploading");
assert.match(stillEncodingBody.detail, /file\/info not playable yet/);

// Byse answers about the code it was asked for, but a provider that switches to
// a list must still be matched by file code rather than by position: an older
// stuck transfer at index 0 would otherwise report a state that never moves.
byseStatusResult = [
  { status: "WORKING", filecode: "someone-elses-file", progress: "3%" },
  { status: "FINISHED", file_code: "byse-file", progress: "100%" },
];
const shadowed = await statusRequest({ operation });
const shadowedBody = await shadowed.json();
assert.equal(shadowedBody.state, "complete");
assert.equal(shadowedBody.url, "https://byse.sx/e/byse-file");

// The same listing without our transfer in it must fall through to file/info
// rather than adopting an unrelated entry's state.
byseStatusResult = [{ status: "WORKING", file_code: "someone-elses-file" }];
bysePlayable = true;
const absent = await statusRequest({ operation });
const absentBody = await absent.json();
assert.equal(absentBody.state, "complete");
assert.equal(absentBody.detail, "remote/status listed no entry");
bysePlayable = false;

// A populated error_msg is the failure channel, even while the status word still
// reads WORKING.
byseStatusResult = { status: "WORKING", progress: "12%", error_msg: "source unreachable" };
const errored = await statusRequest({ operation });
const erroredBody = await errored.json();
assert.equal(erroredBody.state, "failed");
assert.match(erroredBody.detail, /error_msg=source unreachable/);

// The player domain is read from the provider rather than hardcoded, so a
// rotation republishes on the live host without a redeploy. This is the failure
// that silently broke DoodStream: its API kept answering while its embed domain
// moved, so uploads "succeeded" and only the stored links were wrong.
byseStatusResult = { status: "FINISHED", progress: "100%", error_msg: "" };
byseDomain = "moved.example";
const rotated = await statusRequest({ operation });
const rotatedBody = await rotated.json();
assert.equal(rotatedBody.embed_domain, "byse.sx", "cached domain survives within its TTL");

// A domain that is not a bare hostname is refused, because a published link must
// not be redirectable by whatever the provider returns.
assert.equal(rotatedBody.url, "https://byse.sx/e/byse-file");

// A refusal must explain itself without ever echoing the capability URL back.
luluStartResponse = {
  status: 400,
  msg: "invalid url https://files.example.com/lumiere/v1/files/abc/test.mp4",
};
const luluStart = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:remote:lulu",
      provider: "lulustream",
      source_url: "https://files.example.com/lumiere/v1/files/abc/test.mp4",
      filename: "test.mp4",
    }),
  }),
  { ...env, LULUSTREAM_API_KEY: "lulu-key" },
);
assert.equal(luluStart.status, 502);
const luluError = (await luluStart.json()).error;
assert.equal(luluError.code, "provider_rejected");
assert.match(luluError.message, /invalid url <url>/);
assert.ok(!luluError.message.includes("/lumiere/v1/files/"));

// A provider that moves its API answers with a redirect this Worker refuses to
// follow. It must name the new host and nothing else: the Location echoes our
// provider key and the capability URL back at us.
luluStartResponse = "moved";
const luluMoved = await worker.fetch(
  new Request("https://broker.example/v1/remote/start", {
    method: "POST",
    headers: { ...headers, "Content-Type": "application/json" },
    body: JSON.stringify({
      request_id: "test:remote:lulu-moved",
      provider: "lulustream",
      source_url: "https://files.example.com/lumiere/v1/files/abc/test.mp4",
      filename: "test.mp4",
    }),
  }),
  { ...env, LULUSTREAM_API_KEY: "lulu-key" },
);
assert.equal(luluMoved.status, 502);
const movedError = (await luluMoved.json()).error;
assert.equal(movedError.code, "provider_moved");
assert.match(movedError.message, /redirected its API to api\.example\.net/);
assert.ok(!movedError.message.includes("lulu-key"));
assert.ok(!movedError.message.includes("/lumiere/v1/files/"));

globalThis.fetch = realFetch;

console.log("Lumiere Worker contract tests passed");

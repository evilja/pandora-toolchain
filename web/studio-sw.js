"use strict";

self.addEventListener("install", function (event) {
  event.waitUntil(self.skipWaiting());
});

self.addEventListener("activate", function (event) {
  event.waitUntil(self.clients.claim());
});

self.addEventListener("fetch", function (event) {
  var url = new URL(event.request.url);
  if (url.origin !== self.location.origin || url.pathname.indexOf("/studio-media/") !== 0) return;

  event.respondWith((async function () {
    var token = url.searchParams.get("k") || "";
    if (!token) return new Response("missing Studio media token", { status: 401 });

    var match = url.pathname.match(/^\/studio-media\/(source|track)\/(\d+)$/);
    if (!match) return new Response("invalid Studio media path", { status: 404 });
    var apiPath = match[1] === "source"
      ? "/api/v1/studios/current/media/sources/" + match[2]
      : "/api/v1/studios/current/media/tracks/" + match[2];
    var headers = new Headers();
    headers.set("Authorization", "Bearer " + token);
    var range = event.request.headers.get("Range");
    if (range) headers.set("Range", range);
    return fetch(apiPath, {
      method: "GET",
      headers: headers,
      cache: "no-store",
      credentials: "same-origin"
    });
  })());
});

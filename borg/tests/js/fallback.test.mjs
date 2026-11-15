// Tests for postWithFallback (borg/clients/extension/fallback.js).
// Run: node --test borg/tests/js/*.test.mjs   (otto extension-test). No package.json, no deps.
//
// fallback.js is a classic browser script that declares one global function, so
// it is evaluated in a node:vm context carrying node's AbortController, DOMException and timers, and every
// case passes a mock fetchImpl.

import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const FALLBACK_JS = fileURLToPath(new URL("../../clients/extension/fallback.js", import.meta.url));
const FIRST = "http://first.example:8181";
const SECOND = "http://second.example:8181";

function loadPostWithFallback() {
  const context = vm.createContext({ AbortController, DOMException, setTimeout, clearTimeout });
  vm.runInContext(readFileSync(FALLBACK_JS, "utf8"), context, { filename: FALLBACK_JS });
  return context.postWithFallback;
}

const postWithFallback = loadPostWithFallback();

// A mock fetch scripted per URL. Each handler gets (init) and returns a promise.
// Every call is recorded so a case can assert which endpoints were tried.
function mockFetch(handlers) {
  const calls = [];
  const fetchImpl = (url, init) => {
    calls.push({ url, init });
    const handler = handlers[url];
    if (!handler) {
      throw new Error(`unexpected fetch to ${url}`);
    }
    return handler(init);
  };
  return { fetchImpl, calls };
}

const respond = (status) => async () => ({ ok: status >= 200 && status < 300, status });

const networkError = async () => {
  throw new TypeError("NetworkError when attempting to fetch resource.");
};

// Rejects with the signal's reason when it aborts, and never resolves otherwise,
// the way a fetch to a dead peer hangs until the timeout fires.
//
// The keepalive interval stands in for the open socket a real fetch holds, and is
// cleared the moment the abort fires.
const hang = (init) =>
  new Promise((_resolve, reject) => {
    const keepalive = setInterval(() => {}, 1000);
    init.signal.addEventListener(
      "abort",
      () => {
        clearInterval(keepalive);
        reject(init.signal.reason);
      },
      { once: true },
    );
  });

function walk(fetchImpl, extra = {}) {
  return postWithFallback({
    endpoints: [FIRST, SECOND],
    timeoutMs: 50,
    path: "/ingest",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ url: "https://example.com" }),
    fetchImpl,
    ...extra,
  });
}

test("first endpoint rejects with TypeError: second is called and named", async () => {
  const { fetchImpl, calls } = mockFetch({
    [`${FIRST}/ingest`]: networkError,
    [`${SECOND}/ingest`]: respond(200),
  });
  const tried = [];
  const result = await walk(fetchImpl, { onAttempt: (endpoint) => tried.push(endpoint) });

  assert.equal(result.ok, true);
  assert.equal(result.endpoint, SECOND);
  assert.equal(result.response.status, 200);
  assert.deepEqual(calls.map((c) => c.url), [`${FIRST}/ingest`, `${SECOND}/ingest`]);
  assert.deepEqual(tried, [FIRST, SECOND]);
  for (const { init } of calls) {
    assert.equal(init.method, "POST");
    assert.equal("keepalive" in init, false);
  }
});

test("first endpoint times out: second is called", async () => {
  const { fetchImpl, calls } = mockFetch({
    [`${FIRST}/ingest`]: hang,
    [`${SECOND}/ingest`]: respond(200),
  });
  const result = await walk(fetchImpl);

  assert.equal(result.ok, true);
  assert.equal(result.endpoint, SECOND);
  assert.deepEqual(calls.map((c) => c.url), [`${FIRST}/ingest`, `${SECOND}/ingest`]);
});

test("first endpoint returns 500: second is NOT called, result carries the 500", async () => {
  const { fetchImpl, calls } = mockFetch({
    [`${FIRST}/ingest`]: respond(500),
    [`${SECOND}/ingest`]: respond(200),
  });
  const result = await walk(fetchImpl);

  assert.equal(result.ok, true);
  assert.equal(result.endpoint, FIRST);
  assert.equal(result.response.status, 500);
  assert.equal(result.response.ok, false);
  assert.deepEqual(calls.map((c) => c.url), [`${FIRST}/ingest`]);
});

test("first endpoint returns 401: second is NOT called", async () => {
  const { fetchImpl, calls } = mockFetch({
    [`${FIRST}/ingest`]: respond(401),
    [`${SECOND}/ingest`]: respond(200),
  });
  const result = await walk(fetchImpl);

  assert.equal(result.ok, true);
  assert.equal(result.endpoint, FIRST);
  assert.equal(result.response.status, 401);
  assert.deepEqual(calls.map((c) => c.url), [`${FIRST}/ingest`]);
});

test("timeout stops at the response: a slow body is not aborted", async () => {
  let signal;
  const slowBody = (init) => {
    signal = init.signal;
    return Promise.resolve({
      ok: true,
      status: 200,
      json: () =>
        new Promise((resolve, reject) => {
          init.signal.addEventListener("abort", () => reject(init.signal.reason), { once: true });
          setTimeout(() => resolve({ queued: true }), 150);
        }),
    });
  };
  const { fetchImpl } = mockFetch({ [`${FIRST}/ingest`]: slowBody });
  const result = await walk(fetchImpl);

  assert.equal(result.ok, true);
  assert.deepEqual(await result.response.json(), { queued: true });
  assert.equal(signal.aborted, false);
});

test("all endpoints reject: ok is false and attempts lists both in order", async () => {
  const { fetchImpl, calls } = mockFetch({
    [`${FIRST}/ingest`]: networkError,
    [`${SECOND}/ingest`]: hang,
  });
  const result = await walk(fetchImpl);

  assert.equal(result.ok, false);
  assert.deepEqual(calls.map((c) => c.url), [`${FIRST}/ingest`, `${SECOND}/ingest`]);
  // Array.from: the attempts array was built inside the vm context, so its
  // prototype is that realm's Array and deepStrictEqual would reject it.
  assert.deepEqual(Array.from(result.attempts, (a) => a.endpoint), [FIRST, SECOND]);
  assert.equal(result.attempts[0].error, "TypeError: NetworkError when attempting to fetch resource.");
  assert.match(result.attempts[1].error, /^TimeoutError: /);
});

// postWithFallback: POST to each daemon endpoint in order until one answers.
// Classic script, one global function, no DOM, no chrome.* (popup.js calls it;
// borg/tests/js/fallback.test.mjs drives it with a mock fetch). Design:
// docs/plan/2026-11-14-extension-endpoint-fallback.md, API Design.
//
// Rules:
//   - One attempt at a time, in list order, each bounded by
//     AbortSignal.timeout(timeoutMs).
//   - A rejected fetch (network TypeError, TimeoutError, AbortError) is recorded
//     as "<name>: <message>" and the walk moves to the next endpoint.
//   - ANY HTTP response ends the walk, 4xx and 5xx included: the request reached
//     the daemon, and a second route would repeat the answer or double-submit.
//   - NO keepalive (see the popup.js header).
//
// Returns { ok: true, endpoint, response } on the first HTTP response, or
// { ok: false, attempts: [{ endpoint, error }] } when none answered.
async function postWithFallback({ endpoints, timeoutMs, path, headers, body, fetchImpl = fetch, onAttempt = () => {} }) {
  const attempts = [];
  for (const endpoint of endpoints) {
    onAttempt(endpoint);
    try {
      const response = await fetchImpl(endpoint + path, {
        method: "POST",
        headers,
        body,
        signal: AbortSignal.timeout(timeoutMs),
      });
      return { ok: true, endpoint, response };
    } catch (err) {
      attempts.push({ endpoint, error: `${err.name}: ${err.message}` });
    }
  }
  return { ok: false, attempts };
}

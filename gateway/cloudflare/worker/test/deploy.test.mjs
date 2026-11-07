import test from "node:test";
import assert from "node:assert/strict";
import { productionConfig } from "../scripts/prepare-deploy.mjs";
import { verifyAccess } from "../scripts/verify-access.mjs";

const fixture = () => ({
  name: "topvnc-connection-spike", account_id: "a".repeat(32),
  main: "src/index.mjs", compatibility_date: "2026-10-29",
  workers_dev: true, preview_urls: false,
  assets: { directory: "../../../web/dist", run_worker_first: ["/ws"] },
  vars: {
    APP_ORIGIN: "https://topvnc-connection-spike.example.workers.dev",
    ACCESS_ISSUER: "https://example.cloudflareaccess.com", ACCESS_AUD: "test-audience",
    OWNER_EMAIL: "owner@example.com", UPSTREAM_URL: "http://127.0.0.1:6080/ws",
  },
  vpc_services: [{ binding: "HOST_NET", service_id: "12345678-1234-1234-1234-123456789abc", remote: true }],
});

test("deployment configuration strips executable settings and extra infrastructure", () => {
  const config = fixture();
  config.build = { command: "unexpected command" };
  config.routes = ["public.example.com/*"];
  config.vars.EXTRA = "do not publish";
  config.assets.binding = "EXTRA_ASSETS";
  assert.deepEqual(productionConfig(JSON.stringify(config)), fixture());
});

test("deployment rejects missing configuration, changed destination, preview URLs and invalid Access policy", () => {
  for (const input of [undefined, "", "{", "null", "[]"]) assert.throws(() => productionConfig(input));
  const mutations = [
    (c) => { c.name = "another-worker"; },
    (c) => { c.preview_urls = true; },
    (c) => { c.vars.APP_ORIGIN = "https://another.workers.dev"; },
    (c) => { delete c.vars.ACCESS_AUD; },
    (c) => { c.vars.ACCESS_ISSUER = "https://attacker.example.com"; },
    (c) => { c.vars.OWNER_EMAIL = ""; },
    (c) => { c.vars.UPSTREAM_URL = "https://public.example.com/ws"; },
    (c) => { c.vpc_services[0].remote = false; },
    (c) => { c.vpc_services[0].service_id = "-".repeat(36); },
    (c) => { c.vpc_services.push(c.vpc_services[0]); },
    (c) => { c.assets.run_worker_first = ["/*"]; },
  ];
  for (const mutate of mutations) {
    const config = fixture(); mutate(config);
    assert.throws(() => productionConfig(JSON.stringify(config)));
  }
});

test("Access verification checks both page and WebSocket path without following redirects", async () => {
  const { APP_ORIGIN: origin, ACCESS_ISSUER: issuer } = fixture().vars;
  const paths = [];
  await verifyAccess(origin, issuer, async (url, options) => {
    paths.push(url.pathname);
    assert.equal(options.redirect, "manual");
    assert.equal(options.headers, undefined);
    return new Response(null, { status: 302, headers: { location: `${issuer}/cdn-cgi/access/login/example.workers.dev` } });
  });
  assert.deepEqual(paths, ["/", "/ws"]);
  for (const [status, location] of [[200, null], [302, null], [302, "https://attacker.example.com/cdn-cgi/access/login/x"], [302, `${issuer}/other`]]) {
    await assert.rejects(verifyAccess(origin, issuer, async () => new Response(null, { status, headers: location ? { location } : {} })));
  }
  await assert.rejects(verifyAccess(origin, issuer, async (url) => new Response(null, {
    status: url.pathname === "/" ? 302 : 200,
    headers: { location: `${issuer}/cdn-cgi/access/login/example.workers.dev` },
  })));
});

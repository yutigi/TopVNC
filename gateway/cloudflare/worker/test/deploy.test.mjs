import test from "node:test";
import assert from "node:assert/strict";
import { assertPublishable, deploymentConfig } from "../scripts/deployment-config.mjs";
import { verifyAccess } from "../scripts/verify-access.mjs";

const fixture = () => ({
  name: "topvnc-connection-spike", account_id: "a".repeat(32),
  main: "src/index.mjs", compatibility_date: "2026-10-29",
  workers_dev: true, preview_urls: false,
  assets: { directory: "../../../web/dist", run_worker_first: ["/ws"] },
  vars: {
    APP_ORIGIN: "https://topvnc-connection-spike.js-sookudom-temp.workers.dev",
    ACCESS_ISSUER: "https://example.cloudflareaccess.com", ACCESS_AUD: "test-audience",
    OWNER_EMAIL: "owner@example.com", UPSTREAM_URL: "http://127.0.0.1:6080/ws",
  },
  vpc_services: [{ binding: "HOST_NET", service_id: "12345678-1234-1234-1234-123456789abc", remote: true }],
});

const prepare = (config) => deploymentConfig(config, "/checkout/worker", "/checkout/web");

test("manual deployment strips extra infrastructure and always builds this checkout", () => {
  const config = fixture();
  config.build = { command: "unexpected command" };
  config.routes = ["public.example.com/*"];
  config.vars.EXTRA = "do not publish";
  config.assets.binding = "EXTRA_ASSETS";
  config.assets.run_worker_first = ["/*"];
  config.main = "/another-checkout/src/index.mjs";
  const expected = fixture();
  expected.main = "/checkout/worker/src/index.mjs";
  expected.assets.directory = "/checkout/web/dist";
  assert.deepEqual(prepare(config), expected);
});

test("manual deployment rejects placeholders, public upstreams, previews and invalid Access policy", () => {
  for (const input of [undefined, "", null, []]) assert.throws(() => prepare(input));
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
    (c) => { c.vpc_services[0].service_id = "00000000-0000-0000-0000-000000000000"; },
    (c) => { c.vpc_services.push(c.vpc_services[0]); },
    (c) => { c.compatibility_date = "2999-01-01"; },
  ];
  for (const mutate of mutations) {
    const config = fixture(); mutate(config);
    assert.throws(() => prepare(config));
  }
});

test("publishing refuses unmerged, dirty, outdated and unverifiable checkouts", () => {
  const state = { branch: "main", status: "", head: "a".repeat(40), remoteHead: "a".repeat(40) };
  assert.doesNotThrow(() => assertPublishable(state));
  for (const invalid of [
    { branch: "feature" }, { branch: "" }, { status: " M web/app.mjs" },
    { status: "?? unexpected.js" }, { remoteHead: "b".repeat(40) },
    { remoteHead: "" }, { head: "", remoteHead: "" },
  ]) assert.throws(() => assertPublishable({ ...state, ...invalid }));
});

test("publishing refuses a different main commit than the one validated", () => {
  const state = {
    branch: "main", status: "", head: "b".repeat(40), remoteHead: "b".repeat(40),
    checkedHead: "a".repeat(40),
  };
  assert.throws(() => assertPublishable(state), /changed during validation/);
  assert.doesNotThrow(() => assertPublishable({ ...state, checkedHead: state.head }));
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

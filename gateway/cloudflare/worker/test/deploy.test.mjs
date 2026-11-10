import test from "node:test";
import assert from "node:assert/strict";
import { assertPublishable, deploymentConfig } from "../scripts/deployment-config.mjs";
import { verifyAccess } from "../scripts/verify-access.mjs";

const fixture = () => ({
  name: "topvnc-connection-spike", account_id: "a".repeat(32),
  main: "src/index.mjs", compatibility_date: "2026-10-29",
  workers_dev: true, preview_urls: false,
  assets: { directory: "../../../web/dist", run_worker_first: ["/ws", "/ws/*", "/api/hosts"] },
  vars: {
    APP_ORIGIN: "https://topvnc-connection-spike.example.workers.dev",
    ACCESS_ISSUER: "https://example.cloudflareaccess.com", ACCESS_AUD: "test-audience",
    OWNER_EMAIL: "owner@example.com", UPSTREAM_URL: "http://127.0.0.1:6080/ws",
  },
  vpc_services: [{ binding: "HOST_NET", service_id: "12345678-1234-1234-1234-123456789abc", remote: true }],
});

const multiHostFixture = () => {
  const config = fixture();
  delete config.vars.UPSTREAM_URL;
  config.vars.HOSTS_JSON = JSON.stringify([
    { id: "gaming-pc", name: "Gaming PC", binding: "GAMING_NET", upstream: "http://127.0.0.1:6080/ws" },
    { id: "office-pc", name: "Office PC", binding: "OFFICE_NET", upstream: "http://127.0.0.1:6080/ws" },
  ]);
  config.vars.LEGACY_HOST_ID = "gaming-pc";
  config.vpc_services = [
    { binding: "GAMING_NET", service_id: "12345678-1234-1234-1234-123456789abc", remote: true },
    { binding: "OFFICE_NET", service_id: "87654321-4321-4321-4321-cba987654321", remote: true },
  ];
  return config;
};

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
    (c) => { c.vars.LEGACY_HOST_ID = "default"; },
    (c) => { c.vpc_services[0].binding = "OTHER_NET"; },
  ];
  for (const mutate of mutations) {
    const config = fixture(); mutate(config);
    assert.throws(() => prepare(config));
  }
});

test("manual deployment keeps a multi-host registry and one VPC service per machine", () => {
  const config = multiHostFixture();
  config.vars.UPSTREAM_URL = "http://127.0.0.1:6080/ws"; // ignored by the Worker in registry mode
  config.vars.EXTRA = "do not publish";
  config.assets.run_worker_first = ["/ws"];
  const expected = multiHostFixture();
  expected.main = "/checkout/worker/src/index.mjs";
  expected.assets.directory = "/checkout/web/dist";
  assert.deepEqual(prepare(config), expected);

  const withoutMigration = multiHostFixture();
  delete withoutMigration.vars.LEGACY_HOST_ID;
  assert.equal(Object.hasOwn(prepare(withoutMigration).vars, "LEGACY_HOST_ID"), false);
});

test("manual deployment rejects multi-host registries the Worker would refuse or cannot route", () => {
  const hosts = (c) => JSON.parse(c.vars.HOSTS_JSON);
  const setHosts = (c, value) => { c.vars.HOSTS_JSON = JSON.stringify(value); };
  const mutations = [
    (c) => { c.vars.HOSTS_JSON = "not json"; },
    (c) => { setHosts(c, []); },
    (c) => { setHosts(c, hosts(c).map((h) => ({ ...h, id: "gaming-pc" }))); },
    (c) => { setHosts(c, hosts(c).map((h, i) => (i ? { ...h, upstream: "https://public.example.com/ws" } : h))); },
    (c) => { setHosts(c, hosts(c).map((h, i) => (i ? { ...h, upstream: "http://10.0.0.5:6080/ws" } : h))); },
    (c) => { c.vars.LEGACY_HOST_ID = "unknown-pc"; },
    (c) => { c.vpc_services.pop(); },
    (c) => { c.vpc_services.push({ ...c.vpc_services[0], binding: "UNUSED_NET" }); },
    (c) => { c.vpc_services.push(c.vpc_services[0]); },
    (c) => { c.vpc_services[1].remote = false; },
    (c) => { c.vpc_services[1].service_id = "00000000-0000-0000-0000-000000000000"; },
    (c) => { c.vpc_services[0].binding = "HOSTS_JSON"; },
  ];
  for (const [index, mutate] of mutations.entries()) {
    const config = multiHostFixture(); mutate(config);
    assert.throws(() => prepare(config), undefined, `mutation ${index}`);
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

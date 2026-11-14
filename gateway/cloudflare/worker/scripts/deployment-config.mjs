import { AccessPolicy, hostRegistry } from "../src/index.mjs";

const LOOPBACK_UPSTREAM = "http://127.0.0.1:6080/ws";
const ACCESS_VARS = ["APP_ORIGIN", "ACCESS_ISSUER", "ACCESS_AUD", "OWNER_EMAIL"];
const SERVICE_ID = /^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/;

// Keep account details local and deploy this checkout's code/assets even when
// the input config is read from another checkout. Do not execute custom builds
// or create routes, previews, tunnels or other resources from that file.
//
// Two shapes are accepted: the single-host app (UPSTREAM_URL and one HOST_NET
// service) and a multi-host registry (HOSTS_JSON with one VPC service per
// binding it names). Every machine's upstream is its own loopback gateway.
export function deploymentConfig(config, workerDir, webDir) {
  const fail = () => { throw new Error("Local configuration must identify the existing workers.dev app, Access policy and one private VPC service per machine, with preview URLs disabled."); };
  if (!config || typeof config !== "object") fail();
  const services = config.vpc_services;
  if (!/^[a-f0-9]{32}$/.test(config.account_id) ||
      !/^[a-z0-9][a-z0-9-]{0,62}$/.test(config.name) ||
      !/^\d{4}-\d{2}-\d{2}$/.test(config.compatibility_date) ||
      config.compatibility_date > new Date().toISOString().slice(0, 10) ||
      config.workers_dev !== true || config.preview_urls !== false ||
      !config.vars || typeof config.vars !== "object" ||
      !Array.isArray(services) || services.length < 1 || services.length > 32 ||
      services.some((service) => !/^[A-Z][A-Z0-9_]{0,63}$/.test(service?.binding) ||
        Object.hasOwn(config.vars, service.binding) ||
        !SERVICE_ID.test(service.service_id) ||
        service.service_id === "00000000-0000-0000-0000-000000000000" ||
        service.remote !== true)) fail();
  const multiHost = config.vars.HOSTS_JSON !== undefined;
  if (multiHost) {
    let registry;
    try {
      // Validate exactly as the Worker will, with placeholder service bindings.
      const env = Object.fromEntries(services.map(({ binding }) => [binding, { fetch() {} }]));
      registry = hostRegistry({ ...env, HOSTS_JSON: config.vars.HOSTS_JSON, LEGACY_HOST_ID: config.vars.LEGACY_HOST_ID });
    } catch { fail(); }
    const bindings = new Set(registry.hosts.map((host) => host.binding));
    if (registry.hosts.some((host) => host.upstream !== LOOPBACK_UPSTREAM) ||
        bindings.size !== services.length || services.some((service) => !bindings.has(service.binding))) fail();
  } else if (config.vars.UPSTREAM_URL !== LOOPBACK_UPSTREAM || config.vars.LEGACY_HOST_ID !== undefined ||
      services.length !== 1 || services[0].binding !== "HOST_NET") fail();
  try {
    const origin = new URL(config.vars.APP_ORIGIN);
    if (origin.origin !== config.vars.APP_ORIGIN || origin.protocol !== "https:" || origin.port !== "" ||
        !new RegExp(`^${config.name}\\.[a-z0-9-]+\\.workers\\.dev$`).test(origin.hostname)) fail();
    new AccessPolicy(config.vars.ACCESS_ISSUER, config.vars.ACCESS_AUD, config.vars.OWNER_EMAIL);
  } catch { fail(); }
  const routing = multiHost
    ? ["HOSTS_JSON", ...(config.vars.LEGACY_HOST_ID === undefined ? [] : ["LEGACY_HOST_ID"])]
    : ["UPSTREAM_URL"];
  return {
    name: config.name, account_id: config.account_id,
    main: `${workerDir}/src/index.mjs`,
    compatibility_date: config.compatibility_date, workers_dev: true, preview_urls: false,
    // The page always asks the Worker for its machine list, even with one host.
    assets: { directory: `${webDir}/dist`, run_worker_first: ["/ws", "/ws/*", "/api/hosts"] },
    vars: Object.fromEntries([...ACCESS_VARS, ...routing].map((key) => [key, config.vars[key]])),
    vpc_services: services.map(({ binding, service_id }) => ({ binding, service_id, remote: true })),
  };
}

export function assertPublishable({ branch, status, head, remoteHead, checkedHead }) {
  if (branch !== "main") throw new Error("Publishing requires the main branch. Use deploy:check to validate an unmerged branch.");
  if (status.trim()) throw new Error("Publishing requires a clean checkout. Commit or move local changes first.");
  if (!/^[a-f0-9]{40}$/.test(head) || head !== remoteHead) throw new Error("Publishing requires HEAD to match origin's current main. Update main before deploying.");
  if (checkedHead !== undefined && head !== checkedHead) throw new Error("The main commit changed during validation. Run deployment checks again before publishing.");
}

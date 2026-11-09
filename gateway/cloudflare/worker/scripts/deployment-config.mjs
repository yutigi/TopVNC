import { AccessPolicy } from "../src/index.mjs";

// Keep account details local and deploy this checkout's code/assets even when
// the input config is read from another checkout. Do not execute custom builds
// or create routes, previews, tunnels or other resources from that file.
export function deploymentConfig(config, workerDir, webDir) {
  const fail = () => { throw new Error("Local configuration must identify the existing workers.dev app, Access policy and one private VPC service, with preview URLs disabled."); };
  if (!config || typeof config !== "object") fail();
  if (!/^[a-f0-9]{32}$/.test(config.account_id) ||
      !/^[a-z0-9][a-z0-9-]{0,62}$/.test(config.name) ||
      !/^\d{4}-\d{2}-\d{2}$/.test(config.compatibility_date) ||
      config.compatibility_date > new Date().toISOString().slice(0, 10) ||
      config.workers_dev !== true || config.preview_urls !== false ||
      config.vars?.UPSTREAM_URL !== "http://127.0.0.1:6080/ws" ||
      config.vpc_services?.length !== 1 ||
      config.vpc_services[0]?.binding !== "HOST_NET" ||
      !/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(config.vpc_services[0]?.service_id) ||
      config.vpc_services[0]?.service_id === "00000000-0000-0000-0000-000000000000" ||
      config.vpc_services[0]?.remote !== true) fail();
  try {
    const origin = new URL(config.vars.APP_ORIGIN);
    if (origin.origin !== config.vars.APP_ORIGIN || origin.protocol !== "https:" || origin.port !== "" ||
        !new RegExp(`^${config.name}\\.[a-z0-9-]+\\.workers\\.dev$`).test(origin.hostname)) fail();
    new AccessPolicy(config.vars.ACCESS_ISSUER, config.vars.ACCESS_AUD, config.vars.OWNER_EMAIL);
  } catch { fail(); }
  return {
    name: config.name, account_id: config.account_id,
    main: `${workerDir}/src/index.mjs`,
    compatibility_date: config.compatibility_date, workers_dev: true, preview_urls: false,
    assets: { directory: `${webDir}/dist`, run_worker_first: ["/ws"] },
    vars: Object.fromEntries(["APP_ORIGIN", "UPSTREAM_URL", "ACCESS_ISSUER", "ACCESS_AUD", "OWNER_EMAIL"].map((key) => [key, config.vars[key]])),
    vpc_services: [{ binding: "HOST_NET", service_id: config.vpc_services[0].service_id, remote: true }],
  };
}

export function assertPublishable({ branch, status, head, remoteHead, checkedHead }) {
  if (branch !== "main") throw new Error("Publishing requires the main branch. Use deploy:check to validate an unmerged branch.");
  if (status.trim()) throw new Error("Publishing requires a clean checkout. Commit or move local changes first.");
  if (!/^[a-f0-9]{40}$/.test(head) || head !== remoteHead) throw new Error("Publishing requires HEAD to match origin's current main. Update main before deploying.");
  if (checkedHead !== undefined && head !== checkedHead) throw new Error("The main commit changed during validation. Run deployment checks again before publishing.");
}

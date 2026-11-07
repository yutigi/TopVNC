// Account-specific configuration stays in the production environment secret.
// Never interpolate it into shell code or echo it in Actions logs.
import { writeFileSync } from "node:fs";
import { pathToFileURL } from "node:url";
import { AccessPolicy } from "../src/index.mjs";

export function productionConfig(text) {
  let config;
  try { config = JSON.parse(text); } catch { throw new Error("Set CLOUDFLARE_WRANGLER_CONFIG to the existing Worker's JSON configuration."); }
  const fail = () => { throw new Error("Production configuration must preserve the existing Worker, Access policy, assets and private VPC service."); };
  if (!config || typeof config !== "object" || Array.isArray(config)) fail();
  // Rebuild an allowlisted configuration: no custom build commands, routes,
  // migrations, preview URLs or additional infrastructure from the secret.
  if (config.name !== "topvnc-connection-spike" ||
      !/^[a-f0-9]{32}$/.test(config.account_id) ||
      config.main !== "src/index.mjs" ||
      config.compatibility_date !== "2026-10-29" ||
      config.workers_dev !== true || config.preview_urls !== false ||
      config.assets?.directory !== "../../../web/dist" ||
      JSON.stringify(config.assets?.run_worker_first) !== '["/ws"]' ||
      config.vars?.APP_ORIGIN !== "https://topvnc-connection-spike.example.workers.dev" ||
      config.vars?.UPSTREAM_URL !== "http://127.0.0.1:6080/ws" ||
      config.vpc_services?.length !== 1 ||
      config.vpc_services[0].binding !== "HOST_NET" ||
      !/^[a-f0-9]{8}(?:-[a-f0-9]{4}){3}-[a-f0-9]{12}$/.test(config.vpc_services[0].service_id) ||
      config.vpc_services[0].remote !== true) fail();
  try { new AccessPolicy(config.vars.ACCESS_ISSUER, config.vars.ACCESS_AUD, config.vars.OWNER_EMAIL); } catch { fail(); }
  return {
    name: config.name, account_id: config.account_id, main: config.main,
    compatibility_date: config.compatibility_date, workers_dev: true, preview_urls: false,
    assets: { directory: "../../../web/dist", run_worker_first: ["/ws"] },
    vars: Object.fromEntries(["APP_ORIGIN", "UPSTREAM_URL", "ACCESS_ISSUER", "ACCESS_AUD", "OWNER_EMAIL"].map((key) => [key, config.vars[key]])),
    vpc_services: [{ binding: "HOST_NET", service_id: config.vpc_services[0].service_id, remote: true }],
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const config = productionConfig(process.env.CLOUDFLARE_WRANGLER_CONFIG);
  // A multiline secret is masked as a whole; mask individual values too, since
  // Wrangler's binding summary prints them separately.
  if (process.env.GITHUB_ACTIONS === "true") {
    for (const value of [config.account_id, ...Object.values(config.vars), config.vpc_services[0].service_id]) {
      const escaped = value.replaceAll("%", "%25").replaceAll("\r", "%0D").replaceAll("\n", "%0A");
      console.log(`::add-mask::${escaped}`);
    }
  }
  writeFileSync("wrangler.ci.json", JSON.stringify(config, null, 2), { mode: 0o600 });
}

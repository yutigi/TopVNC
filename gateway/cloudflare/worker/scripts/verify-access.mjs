import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

export async function verifyAccess(origin, issuer, request = fetch) {
  for (const path of ["/", "/ws"]) {
    const response = await request(new URL(path, origin), { redirect: "manual", signal: AbortSignal.timeout(15000) });
    const location = response.headers.get("location");
    let login;
    try { login = new URL(location); } catch { /* Reject absent/invalid redirects below. */ }
    await response.body?.cancel();
    if (response.status !== 302 || login?.origin !== new URL(issuer).origin || !login.pathname.startsWith("/cdn-cgi/access/login/")) {
      throw new Error(`Access protection check failed for ${path}: expected a 302 to the configured Access login.`);
    }
    console.log(`Access protection verified for ${path}.`);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  const { vars } = JSON.parse(readFileSync("wrangler.ci.json", "utf8"));
  await verifyAccess(vars.APP_ORIGIN, vars.ACCESS_ISSUER);
}

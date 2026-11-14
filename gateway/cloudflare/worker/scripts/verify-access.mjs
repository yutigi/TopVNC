// Use the validated deployment variables so every configured route is checked.
export async function verifyAccess({ APP_ORIGIN: origin, ACCESS_ISSUER: issuer, HOSTS_JSON: hosts }, request = fetch) {
  const paths = ["/", "/api/hosts", ...(hosts === undefined
    ? ["/ws"]
    : JSON.parse(hosts).map(({ id }) => `/ws/${id}`))];
  for (const path of paths) {
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

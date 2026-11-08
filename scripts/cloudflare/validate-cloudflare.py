"""Probe unauthenticated Access redirects, then dry-run the configured Worker."""
import argparse
import json
import re
import sys
import tomllib
import urllib.error
import urllib.parse
import urllib.request

from common import CONFIG, run_wrangler


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        return None


def probe_paths(variables):
    if "HOSTS_JSON" not in variables:
        return ["/", "/ws"]
    hosts = json.loads(variables["HOSTS_JSON"])
    if not isinstance(hosts, list) or not 1 <= len(hosts) <= 32:
        raise ValueError("HOSTS_JSON must contain 1 to 32 hosts")
    ids = [host.get("id") if isinstance(host, dict) else None for host in hosts]
    if any(not isinstance(host_id, str) or
           re.fullmatch(r"[a-z0-9][a-z0-9-]{0,47}", host_id) is None for host_id in ids):
        raise ValueError("HOSTS_JSON contains an invalid host ID")
    if len(ids) != len(set(ids)):
        raise ValueError("HOSTS_JSON contains duplicate host IDs")
    paths = ["/", "/api/hosts", *[f"/ws/{host_id}" for host_id in ids]]
    if variables.get("LEGACY_HOST_ID"):
        paths.append("/ws")
    return paths


def access_redirect(status, location):
    try:
        target = urllib.parse.urlsplit(location)
        hostname = target.hostname or ""
        return (status == 302 and target.scheme == "https" and
                hostname.endswith(".cloudflareaccess.com") and
                target.username is None and target.password is None and
                target.port in (None, 443))
    except ValueError:
        return False


def main():
    argparse.ArgumentParser(description=__doc__).parse_args()
    variables = tomllib.loads(CONFIG.read_text(encoding="utf-8"))["vars"]
    origin = variables["APP_ORIGIN"]
    parsed = urllib.parse.urlsplit(origin)
    if (parsed.scheme != "https" or not parsed.hostname or parsed.username or
            parsed.password or parsed.path or parsed.query or parsed.fragment):
        sys.exit("APP_ORIGIN must be an exact HTTPS origin")
    paths = probe_paths(variables)
    opener = urllib.request.build_opener(NoRedirect)
    for path in paths:
        try:
            request = urllib.request.Request(origin + path, headers={"User-Agent": "Mozilla/5.0"})
            response = opener.open(request, timeout=20)
        except urllib.error.HTTPError as response_error:
            response = response_error
        protected = access_redirect(response.code, response.headers.get("Location", ""))
        print(json.dumps({"path": path, "status": response.code,
                          "redirects_to_access": protected}), flush=True)
        response.close()
        if not protected:
            sys.exit("Access protection could not be confirmed; dry run skipped")
    return run_wrangler(["deploy", "--dry-run"], timeout=90)


if __name__ == "__main__":
    sys.exit(main())

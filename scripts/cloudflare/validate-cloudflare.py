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


# Match verify-access.mjs: the page needs /api/hosts in both modes, and a
# registry routes only /ws/<id>. LEGACY_HOST_ID affects browser storage only.
def probe_paths(variables):
    if "HOSTS_JSON" not in variables:
        return ["/", "/api/hosts", "/ws"]
    hosts = json.loads(variables["HOSTS_JSON"])
    if not isinstance(hosts, list) or not 1 <= len(hosts) <= 32:
        raise ValueError("HOSTS_JSON must contain 1 to 32 hosts")
    ids = [host.get("id") if isinstance(host, dict) else None for host in hosts]
    if any(not isinstance(host_id, str) or
           re.fullmatch(r"[a-z0-9][a-z0-9-]{0,47}", host_id) is None for host_id in ids):
        raise ValueError("HOSTS_JSON contains an invalid host ID")
    if len(ids) != len(set(ids)):
        raise ValueError("HOSTS_JSON contains duplicate host IDs")
    return ["/", "/api/hosts", *[f"/ws/{host_id}" for host_id in ids]]


def access_team(issuer):
    """The ACCESS_ISSUER hostname, or None unless it is an HTTPS Access team URL."""
    try:
        parts = urllib.parse.urlsplit(issuer)
        valid = (parts.scheme == "https" and parts.port in (None, 443) and
                 parts.username is None and parts.password is None and
                 (parts.hostname or "").endswith(".cloudflareaccess.com") and
                 parts.path in ("", "/") and not parts.query and not parts.fragment)
        return parts.hostname if valid else None
    except (TypeError, ValueError, AttributeError):
        return None


def access_redirect(status, location, team):
    """True only for a 302 to the configured team's Access login, as verify-access.mjs requires."""
    try:
        target = urllib.parse.urlsplit(location)
        return (status == 302 and team is not None and target.scheme == "https" and
                target.hostname == team and target.port in (None, 443) and
                target.username is None and target.password is None and
                target.path.startswith("/cdn-cgi/access/login/"))
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
    team = access_team(variables.get("ACCESS_ISSUER"))
    if team is None:
        sys.exit("ACCESS_ISSUER must be your https://<team>.cloudflareaccess.com URL")
    paths = probe_paths(variables)
    opener = urllib.request.build_opener(NoRedirect)
    for path in paths:
        try:
            request = urllib.request.Request(origin + path, headers={"User-Agent": "Mozilla/5.0"})
            response = opener.open(request, timeout=20)
        except urllib.error.HTTPError as response_error:
            response = response_error
        protected = access_redirect(response.code, response.headers.get("Location", ""), team)
        print(json.dumps({"path": path, "status": response.code,
                          "redirects_to_access": protected}), flush=True)
        response.close()
        if not protected:
            sys.exit("Access protection could not be confirmed; dry run skipped")
    return run_wrangler(["deploy", "--dry-run"], timeout=90)


if __name__ == "__main__":
    sys.exit(main())

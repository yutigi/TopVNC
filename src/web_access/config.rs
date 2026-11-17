//! Where the browser-access files are and whether they are ready to run.
//! The checks mirror `Gateway::new` in crates/topvnc-web-gateway, so the WEB
//! tab refuses a configuration the gateway would refuse, before anything
//! starts.

use serde::Deserialize;
use std::ffi::OsString;
use std::fmt;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

/// The config directory, relative to the repository root.
const CLOUDFLARE_DIR: &str = "gateway/cloudflare";
const EXAMPLE_CONFIG: &str = "gateway.example.toml";
const LOCAL_CONFIG: &str = "gateway.local.toml";
const TOKEN_FILE: &str = "tunnel-token.txt";
/// Larger files are not a gateway configuration.
const MAX_CONFIG_BYTES: u64 = 16 * 1024;

/// A reason the WEB tab cannot start, with the runbook/Setup step that
/// fixes it (`docs/cloudflare-gateway.md`, "One-time setup").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub message: String,
    pub step: Option<u8>,
}

impl Problem {
    fn new(message: impl Into<String>, step: Option<u8>) -> Self {
        Self {
            message: message.into(),
            step,
        }
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Some(step) => write!(f, "{} (setup step {step})", self.message),
            None => f.write_str(&self.message),
        }
    }
}

/// The gateway configuration file, with the gateway's own field names.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GatewayFile {
    listen: SocketAddr,
    rfb_target: SocketAddr,
    app_origin: String,
    #[serde(default)]
    host_id: Option<String>,
    issuer: String,
    audience: String,
    owner_email: String,
    max_session_seconds: u64,
    host_key_file: PathBuf,
    /// Listed so the file parses; only the gateway uses it.
    #[allow(dead_code)]
    browser_keys_file: PathBuf,
}

/// What the WEB tab needs from a valid `gateway.local.toml`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewaySettings {
    pub listen: SocketAddr,
    pub rfb_target: SocketAddr,
    pub app_origin: String,
    /// Resolved relative to the configuration file, as the gateway does.
    pub host_key_file: PathBuf,
}

/// Values copied unchanged from the example files.
fn placeholder(value: &str) -> bool {
    value.contains("YOUR_") || value.eq_ignore_ascii_case("you@example.com")
}

/// Parse and check a gateway configuration without touching the disk.
pub fn parse_gateway(text: &str, base: &Path) -> Result<GatewaySettings, Problem> {
    let file: GatewayFile = toml::from_str(text).map_err(|error| {
        Problem::new(
            format!(
                "{LOCAL_CONFIG} is not valid: {}",
                error.message().trim_end()
            ),
            Some(3),
        )
    })?;
    if placeholder(&file.app_origin) {
        return Err(Problem::new(
            "app_origin still holds the example placeholder",
            Some(3),
        ));
    }
    topvnc_access::validate_origin(&file.app_origin).map_err(|reason| {
        Problem::new(
            format!("app_origin: {reason}, lowercase, with no trailing slash"),
            Some(3),
        )
    })?;
    if let Some(id) = &file.host_id {
        topvnc_access::validate_host_id(id).map_err(|reason| Problem::new(reason, Some(3)))?;
    }
    if [&file.issuer, &file.audience, &file.owner_email]
        .iter()
        .any(|value| placeholder(value))
    {
        return Err(Problem::new(
            "issuer, audience, or owner_email still holds an example placeholder",
            Some(4),
        ));
    }
    topvnc_access::AccessPolicy::new(&file.issuer, &file.audience, &file.owner_email).map_err(
        |reason| {
            Problem::new(
                format!("{reason}: check issuer, audience, and owner_email"),
                Some(4),
            )
        },
    )?;
    if !file.listen.ip().is_loopback()
        || !file.rfb_target.ip().is_loopback()
        || file.rfb_target.port() == 0
    {
        return Err(Problem::new(
            "listen and rfb_target must be loopback addresses, with an RFB port",
            Some(3),
        ));
    }
    if !(1..=7200).contains(&file.max_session_seconds) {
        return Err(Problem::new("max_session_seconds must be 1-7200", Some(3)));
    }
    Ok(GatewaySettings {
        listen: file.listen,
        rfb_target: file.rfb_target,
        app_origin: file.app_origin,
        host_key_file: base.join(file.host_key_file),
    })
}

/// Everything needed to start browser access, after every local check passed.
#[derive(Clone, Debug)]
pub struct Launch {
    pub settings: GatewaySettings,
    pub gateway_config: PathBuf,
    pub gateway_program: PathBuf,
    pub token_file: PathBuf,
    pub cloudflared: PathBuf,
}

/// The repository checkout that holds `gateway/cloudflare`, looked up from
/// the executable's folders (as `target/release/topvnc.exe`) and then the
/// source directory this binary was built from.
pub fn find_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    let ancestors = exe
        .iter()
        .flat_map(|exe| exe.ancestors().skip(1).take(4))
        .map(Path::to_path_buf);
    ancestors
        .chain(std::iter::once(PathBuf::from(env!("CARGO_MANIFEST_DIR"))))
        .find(|root| root.join(CLOUDFLARE_DIR).join(EXAMPLE_CONFIG).is_file())
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

/// `CLOUDFLARED` if set (as the console wrapper allows), otherwise the first
/// `cloudflared` on `PATH`.
fn find_cloudflared(variable: Option<OsString>, path: Option<OsString>) -> Option<PathBuf> {
    if let Some(program) = variable.filter(|value| !value.is_empty()) {
        let program = PathBuf::from(program);
        return program.is_file().then_some(program);
    }
    std::env::split_paths(&path?)
        .map(|dir| dir.join(executable("cloudflared")))
        .find(|candidate| candidate.is_file())
}

/// Check the files and programs browser access needs, in setup order.
pub fn prepare() -> Result<Launch, Problem> {
    let root = find_root().ok_or_else(|| {
        Problem::new(
            format!("TopVNC could not find the repository's {CLOUDFLARE_DIR} folder next to it"),
            None,
        )
    })?;
    let dir = root.join(CLOUDFLARE_DIR);
    let gateway_config = dir.join(LOCAL_CONFIG);
    let text = match std::fs::metadata(&gateway_config) {
        Ok(meta) if meta.len() > MAX_CONFIG_BYTES => {
            return Err(Problem::new(
                format!("{LOCAL_CONFIG} is too large"),
                Some(3),
            ));
        }
        Ok(_) => std::fs::read_to_string(&gateway_config).map_err(|error| {
            Problem::new(format!("cannot read {LOCAL_CONFIG}: {error}"), Some(3))
        })?,
        Err(_) => {
            return Err(Problem::new(
                format!("{CLOUDFLARE_DIR}/{LOCAL_CONFIG} is missing"),
                Some(3),
            ));
        }
    };
    let settings = parse_gateway(&text, &dir)?;
    let token_file = dir.join(TOKEN_FILE);
    // Only its presence is checked; the token is never read here.
    if !token_file.is_file() {
        return Err(Problem::new(
            format!("{CLOUDFLARE_DIR}/{TOKEN_FILE} is missing"),
            Some(5),
        ));
    }
    if !settings.host_key_file.is_file() {
        return Err(Problem::new(
            "the host key file is missing; create it with --init-host-key",
            Some(7),
        ));
    }
    let gateway_program = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(executable("topvnc-web-gateway"))))
        .filter(|program| program.is_file())
        .ok_or_else(|| {
            Problem::new(
                "topvnc-web-gateway is not built next to TopVNC: run cargo build --release -p topvnc-web-gateway (or without --release for a debug TopVNC)",
                None,
            )
        })?;
    let cloudflared = find_cloudflared(std::env::var_os("CLOUDFLARED"), std::env::var_os("PATH"))
        .ok_or_else(|| {
        Problem::new(
            "cloudflared was not found: install it, or set CLOUDFLARED to its full path",
            Some(1),
        )
    })?;
    Ok(Launch {
        settings,
        gateway_config,
        gateway_program,
        token_file,
        cloudflared,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
listen = "127.0.0.1:6080"
rfb_target = "127.0.0.1:5900"
app_origin = "https://topvnc.owner.workers.dev"
issuer = "https://team.cloudflareaccess.com"
audience = "0123456789abcdef"
owner_email = "owner@example.org"
max_session_seconds = 7200
host_key_file = "host.key"
browser_keys_file = "browsers.txt"
"#;

    fn with(key: &str, value: &str) -> String {
        VALID
            .lines()
            .map(|line| {
                if line.starts_with(&format!("{key} =")) {
                    format!("{key} = {value}")
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_complete_config_is_accepted_with_paths_beside_it() {
        let settings = parse_gateway(VALID, Path::new("base")).unwrap();
        assert_eq!(settings.listen, "127.0.0.1:6080".parse().unwrap());
        assert_eq!(settings.rfb_target, "127.0.0.1:5900".parse().unwrap());
        assert_eq!(settings.host_key_file, Path::new("base").join("host.key"));
    }

    #[test]
    fn optional_machine_id_matches_gateway_rules() {
        assert!(
            parse_gateway(&format!("{VALID}\nhost_id = \"gaming-pc\""), Path::new(".")).is_ok()
        );
        for id in [
            "",
            "UPPER",
            "-bad",
            "a/b",
            "a?b",
            "a#b",
            "ไทย",
            &"x".repeat(49),
        ] {
            let problem =
                parse_gateway(&format!("{VALID}\nhost_id = \"{id}\""), Path::new(".")).unwrap_err();
            assert_eq!(problem.step, Some(3));
            assert!(problem.message.contains("host_id"));
        }
    }

    #[test]
    fn the_example_file_is_refused_until_filled_in() {
        let example = include_str!("../../gateway/cloudflare/gateway.example.toml");
        let problem = parse_gateway(example, Path::new(".")).unwrap_err();
        assert_eq!(problem.step, Some(3), "{problem}");
        let origin_fixed = example.replace(
            "https://topvnc.YOUR_SUBDOMAIN.workers.dev",
            "https://topvnc.owner.workers.dev",
        );
        let problem = parse_gateway(&origin_fixed, Path::new(".")).unwrap_err();
        assert_eq!(problem.step, Some(4), "{problem}");
    }

    #[test]
    fn gateway_rules_are_enforced() {
        for (key, value, step) in [
            ("app_origin", r#""https://topvnc.owner.workers.dev/""#, 3),
            ("app_origin", r#""https://TopVNC.owner.workers.dev""#, 3),
            ("app_origin", r#""https://example.com""#, 3),
            ("issuer", r#""https://team.example.com""#, 4),
            ("owner_email", r#""you@example.com""#, 4),
            ("owner_email", r#""nobody""#, 4),
            ("listen", r#""0.0.0.0:6080""#, 3),
            ("rfb_target", r#""192.168.1.5:5900""#, 3),
            ("rfb_target", r#""127.0.0.1:0""#, 3),
            ("max_session_seconds", "0", 3),
            ("max_session_seconds", "7201", 3),
        ] {
            let problem = parse_gateway(&with(key, value), Path::new(".")).unwrap_err();
            assert_eq!(problem.step, Some(step), "{key} = {value}: {problem}");
        }
    }

    #[test]
    fn unknown_or_missing_fields_are_refused() {
        let extra = format!("{VALID}\nsecret = \"x\"\n");
        assert_eq!(
            parse_gateway(&extra, Path::new(".")).unwrap_err().step,
            Some(3)
        );
        let missing = VALID.replace("listen = \"127.0.0.1:6080\"", "");
        assert_eq!(
            parse_gateway(&missing, Path::new(".")).unwrap_err().step,
            Some(3)
        );
    }

    #[test]
    fn problems_name_their_setup_step() {
        assert_eq!(
            Problem::new("x is missing", Some(5)).to_string(),
            "x is missing (setup step 5)"
        );
        assert_eq!(Problem::new("plain", None).to_string(), "plain");
    }

    #[test]
    fn cloudflared_comes_from_the_variable_before_path() {
        let missing = std::env::temp_dir().join("topvnc-no-such-cloudflared.exe");
        // A set variable is authoritative, even when it points nowhere.
        assert_eq!(
            find_cloudflared(Some(missing.into_os_string()), std::env::var_os("PATH")),
            None
        );
        let dir = std::env::temp_dir().join(format!("topvnc-cloudflared-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let program = dir.join(executable("cloudflared"));
        std::fs::write(&program, b"").unwrap();
        let path = std::env::join_paths([dir.clone()]).unwrap();
        assert_eq!(
            find_cloudflared(None, Some(path.clone())),
            Some(program.clone())
        );
        assert_eq!(
            find_cloudflared(Some(OsString::new()), Some(path)),
            Some(program)
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}

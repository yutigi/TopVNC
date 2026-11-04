//! Phase 0 only: authenticate and forward an upgrade, without accepting its socket.

use futures_util::{
    StreamExt,
    future::{Either, select},
};
use std::{cell::RefCell, rc::Rc, time::Duration};
use topvnc_spike_policy::{
    AccessPolicy, KEY_REFRESH_SECONDS, KEY_TTL_SECONDS, KeySet, MAX_JWKS_BYTES, token_key_id,
    validate_upgrade, validate_upstream,
};
use wasm_bindgen::{JsCast, prelude::*};
use worker::{
    Context, Env, Fetch, Headers, Method, Request, RequestInit, RequestRedirect, Response, Result,
    event,
};

// workers-rs 0.8.7 has no typed VPC binding. Bind only the documented fetch API.
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = js_sys::Object)]
    type PrivateService;

    #[wasm_bindgen(method, catch, js_name = fetch)]
    fn fetch_private(
        this: &PrivateService,
        request: &web_sys::Request,
    ) -> std::result::Result<js_sys::Promise, JsValue>;
}

#[derive(Default)]
struct Cache {
    keys: Option<Rc<KeySet>>,
    loaded_at: u64,
    last_attempt: Option<u64>,
}

thread_local! { static KEYS: RefCell<Cache> = RefCell::new(Cache::default()); }

fn now() -> u64 {
    (js_sys::Date::now() / 1000.0) as u64
}

fn failure(message: &str) -> worker::Error {
    worker::Error::RustError(message.into())
}

async fn signing_keys(policy: &AccessPolicy, kid: &str) -> Result<Rc<KeySet>> {
    let time = now();
    let cached = KEYS.with(|cache| {
        let cache = cache.borrow();
        cache
            .keys
            .as_ref()
            .filter(|keys| {
                time.saturating_sub(cache.loaded_at) < KEY_TTL_SECONDS && keys.contains(kid)
            })
            .cloned()
    });
    if let Some(keys) = cached {
        return Ok(keys);
    }
    let refresh = KEYS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache
            .last_attempt
            .is_some_and(|last| time.saturating_sub(last) < KEY_REFRESH_SECONDS)
        {
            return false;
        }
        cache.last_attempt = Some(time);
        true
    });
    if !refresh {
        return Err(failure("Signing key refresh rate limited"));
    }
    let init = RequestInit {
        redirect: RequestRedirect::Manual,
        ..RequestInit::default()
    };
    let request = Request::new_with_init(&policy.jwks_url(), &init)?;
    let signal = worker::AbortSignal::from(web_sys::AbortSignal::timeout_with_u32(5000));
    let mut response = Fetch::Request(request).send_with_signal(&signal).await?;
    if response.status_code() != 200 {
        return Err(failure("Signing keys unavailable"));
    }
    if response
        .headers()
        .get("Content-Length")?
        .and_then(|n| n.parse::<usize>().ok())
        .is_some_and(|n| n > MAX_JWKS_BYTES)
    {
        return Err(failure("JWKS too large"));
    }
    let mut stream = response.stream()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        if chunk.len() > MAX_JWKS_BYTES.saturating_sub(bytes.len()) {
            return Err(failure("JWKS too large"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let keys = Rc::new(KeySet::parse(&bytes).map_err(failure)?);
    KEYS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.keys = Some(keys.clone());
        cache.loaded_at = time;
    });
    Ok(keys)
}

#[event(fetch)]
pub async fn main(request: Request, env: Env, _ctx: Context) -> Result<Response> {
    match handle(request, env).await {
        Ok(response) => Ok(response),
        Err(_) => {
            // Never log request headers, JWTs, upstream response bodies, or credentials.
            worker::console_error!(
                "Phase 0 upgrade failed; inspect configuration and private connectivity"
            );
            Response::error("Private gateway unavailable", 503)
        }
    }
}

async fn handle(request: Request, env: Env) -> Result<Response> {
    let url = request.url()?;
    if url.path() != "/ws" {
        return Response::error("Not found", 404);
    }
    let app_origin = env.var("APP_ORIGIN")?.to_string();
    let headers = request.headers();
    let origin = headers.get("Origin")?;
    let protocols = headers.get("Sec-WebSocket-Protocol")?;
    let upgrade = headers.get("Upgrade")?;
    if url.origin().ascii_serialization() != app_origin
        || validate_upgrade(
            request.method().as_ref(),
            url.path(),
            url.query(),
            origin.as_deref(),
            protocols.as_deref(),
            upgrade.as_deref(),
            &app_origin,
        )
        .is_err()
    {
        return Response::error("WebSocket request rejected", 403);
    }
    let Some(token) = headers.get("Cf-Access-Jwt-Assertion")? else {
        return Response::error("Access authentication required", 401);
    };
    let policy = AccessPolicy::new(
        &env.var("ACCESS_ISSUER")?.to_string(),
        &env.var("ACCESS_AUD")?.to_string(),
        &env.var("OWNER_EMAIL")?.to_string(),
    )
    .map_err(failure)?;
    let kid = match token_key_id(&token) {
        Ok(kid) => kid,
        Err(_) => return Response::error("Access authentication failed", 401),
    };
    let keys = signing_keys(&policy, &kid).await?;
    if keys.verify(&token, &policy, now()).is_err() {
        return Response::error("Access authentication failed", 401);
    }
    let upstream = env.var("UPSTREAM_URL")?.to_string();
    validate_upstream(&upstream).map_err(failure)?;
    let outbound_headers = Headers::new();
    for (name, value) in [
        ("Upgrade", "websocket"),
        ("Sec-WebSocket-Protocol", "binary"),
        ("Origin", &app_origin),
        ("Cf-Access-Jwt-Assertion", &token),
    ] {
        outbound_headers.set(name, value)?;
    }
    let init = RequestInit {
        method: Method::Get,
        headers: outbound_headers,
        redirect: RequestRedirect::Manual,
        ..RequestInit::default()
    };
    let outbound = Request::new_with_init(&upstream, &init)?;
    let raw = web_sys::Request::try_from(outbound)?;
    // Abort only a pending handshake; do not attach a timeout signal to a live session.
    let controller = web_sys::AbortController::new()?;
    let raw_init = web_sys::RequestInit::new();
    raw_init.set_signal(Some(&controller.signal()));
    let raw = web_sys::Request::new_with_request_and_init(&raw, &raw_init)?;
    let binding = js_sys::Reflect::get(&env, &JsValue::from_str("HOST_NET"))?;
    if !js_sys::Reflect::get(&binding, &JsValue::from_str("fetch"))?.is_function() {
        return Err(failure("HOST_NET has no fetch API"));
    }
    let binding: PrivateService = binding.unchecked_into();
    let pending = Box::pin(wasm_bindgen_futures::JsFuture::from(
        binding.fetch_private(&raw)?,
    ));
    let deadline = Box::pin(worker::Delay::from(Duration::from_secs(5)));
    let response = match select(pending, deadline).await {
        Either::Left((response, _)) => response?,
        Either::Right(_) => {
            controller.abort();
            return Err(failure("Private upgrade timed out"));
        }
    };
    let response: web_sys::Response = response.dyn_into()?;
    if response.status() != 101 {
        return Response::error("Private gateway refused upgrade", 502);
    }
    // Preserve the runtime WebSocket response. No accept(), message listener, or JS byte loop.
    Ok(Response::from(response))
}

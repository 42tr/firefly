use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, FromRequestParts},
    http::{HeaderMap, StatusCode},
    response::Response,
    Json,
};
use hmac::{Hmac, Mac};
use http::request::Parts;
use serde::Deserialize;
use sha2::Sha256;
use tower_cookies::{
    cookie::{time::Duration as CookieDuration, SameSite},
    Cookie, Cookies,
};

use super::{error::AppError, jwt};
use crate::{config, db};

const SESSION_COOKIE: &str = "firefly_session";
const LOGIN_BAN_DURATION: Duration = Duration::from_secs(60 * 60);
const LOGIN_FAILURE_LIMIT: u8 = 3;

pub(super) fn check_origin(headers: &HeaderMap) -> Result<(), AppError> {
    let origin =
        headers.get("origin").and_then(|v| v.to_str().ok()).ok_or_else(|| AppError::forbidden("Missing Origin"))?;
    let cfg = config::get();
    let allowed = std::iter::once(cfg.site_url.as_str())
        .chain(cfg.site_url_aliases.split(','))
        .chain(cfg.cors_allowed_origins.split(','));
    if !allowed
        .filter_map(|v| reqwest::Url::parse(v.trim()).ok())
        .any(|url| url.origin().ascii_serialization() == origin)
    {
        return Err(AppError::forbidden("Unrecognized Origin"));
    }
    Ok(())
}

/// Serve the bundled local login form, including on the old /auth/ entry point.
pub async fn page(headers: HeaderMap) -> Response {
    super::frontend_router(http::Uri::from_static("/"), headers).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LoginRequest {
    password: String,
}

#[derive(Default)]
struct LoginAttempts(HashMap<IpAddr, LoginAttempt>);

enum LoginAttempt {
    Failures(u8),
    BannedUntil(Instant),
}

impl LoginAttempts {
    // The caller holds the mutex across checking the ban and verifying the password,
    // so concurrent requests cannot slip past the third failed attempt.
    fn authenticate(
        &mut self,
        ip: IpAddr,
        now: Instant,
        verify_password: impl FnOnce() -> bool,
    ) -> Result<(), AppError> {
        let ip = ip.to_canonical();
        self.0.retain(
            |_, attempt| !matches!(attempt, LoginAttempt::BannedUntil(until) if *until <= now),
        );
        if let Some(LoginAttempt::BannedUntil(until)) = self.0.get(&ip) {
            let remaining = until.duration_since(now);
            return Err(Self::banned_error(
                remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0),
            ));
        }
        if verify_password() {
            self.0.remove(&ip);
            return Ok(());
        }
        let failures = match self.0.get(&ip) {
            Some(LoginAttempt::Failures(count)) => count + 1,
            _ => 1,
        };
        if failures >= LOGIN_FAILURE_LIMIT {
            self.0
                .insert(ip, LoginAttempt::BannedUntil(now + LOGIN_BAN_DURATION));
            return Err(Self::banned_error(LOGIN_BAN_DURATION.as_secs()));
        }
        self.0.insert(ip, LoginAttempt::Failures(failures));
        Err(AppError::unauthorized("访问密码错误"))
    }

    fn banned_error(retry_after_secs: u64) -> AppError {
        AppError::too_many_requests(
            "登录失败次数过多，该 IP 已封禁，请稍后重试",
            retry_after_secs,
        )
    }
}

// Compare fixed-length keyed digests in constant time; credentials stay in server configuration.
fn password_matches(password: &str, cfg: &config::Config) -> bool {
    let mut expected =
        Hmac::<Sha256>::new_from_slice(cfg.jwt_secret.as_bytes()).expect("HMAC accepts any key");
    expected.update(cfg.auth_password.as_bytes());
    let expected = expected.finalize().into_bytes();
    let mut candidate =
        Hmac::<Sha256>::new_from_slice(cfg.jwt_secret.as_bytes()).expect("HMAC accepts any key");
    candidate.update(password.as_bytes());
    candidate.verify_slice(&expected).is_ok()
}

pub async fn login(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    cookies: Cookies,
    headers: HeaderMap,
    Json(request): Json<LoginRequest>,
) -> Result<StatusCode, AppError> {
    check_origin(&headers)?;
    let cfg = config::get();
    static ATTEMPTS: OnceLock<Mutex<LoginAttempts>> = OnceLock::new();
    ATTEMPTS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .authenticate(peer.ip(), Instant::now(), || {
            password_matches(&request.password, &cfg)
        })?;
    let token = jwt::create(&jwt::Claims::new(7 * 24))
        .map_err(|_| AppError::internal_server_error("无法创建登录会话"))?;
    cookies.add(
        Cookie::build((SESSION_COOKIE, token))
            .path("/")
            .max_age(CookieDuration::days(7))
            .http_only(true)
            .secure(cfg.cookie_secure)
            .same_site(SameSite::Lax)
            .build(),
    );
    // Remove obsolete OAuth state when upgrading an existing browser.
    cookies.remove(Cookie::build("firefly_login").path("/auth").build());
    Ok(StatusCode::NO_CONTENT)
}

pub async fn logout(
    _session: Session,
    cookies: Cookies,
    headers: HeaderMap,
) -> Result<StatusCode, AppError> {
    check_origin(&headers)?;
    cookies.remove(Cookie::build(SESSION_COOKIE).path("/").build());
    Ok(StatusCode::NO_CONTENT)
}

/// A local session has no account identity. The legacy schema uses one fixed storage scope.
pub struct Session {
    scope: String,
}

impl Session {
    pub fn storage_scope(&self) -> &str {
        &self.scope
    }

    pub(super) fn for_storage(scope: &str) -> Self {
        Self {
            scope: scope.into(),
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        validate_auth(parts)?;
        Ok(Self {
            scope: db::SINGLE_SCOPE.into(),
        })
    }
}

fn validate_auth(parts: &Parts) -> Result<jwt::Claims, AppError> {
    let cookie_header = parts
        .headers
        .get("cookie")
        .and_then(|hv| hv.to_str().ok())
        .unwrap_or("");
    let token = cookie_header
        .split(';')
        .find_map(|cookie| cookie.trim().strip_prefix("firefly_session="))
        .ok_or_else(|| AppError::unauthorized("请先登录"))?;
    jwt::validate(token).map_err(|_| AppError::unauthorized("登录已过期，请重新登录"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_are_exact_and_empty_passwords_fail() {
        config::init_test_config();
        let cfg = config::get();
        assert!(password_matches(&cfg.auth_password, &cfg));
        assert!(!password_matches("", &cfg));
        assert!(!password_matches(&format!("{} ", cfg.auth_password), &cfg));
    }

    #[test]
    fn third_failure_bans_for_exactly_one_hour_without_extending_on_retry() {
        let now = Instant::now();
        let ip = "192.0.2.1".parse().unwrap();
        let mut attempts = LoginAttempts::default();
        for _ in 0..2 {
            assert_eq!(
                attempts
                    .authenticate(ip, now, || false)
                    .unwrap_err()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        use axum::response::IntoResponse;
        let banned = attempts
            .authenticate(ip, now, || false)
            .unwrap_err()
            .into_response();
        assert_eq!(banned.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(banned.headers()["retry-after"], "3600");
        let retry = attempts
            .authenticate(
                ip,
                now + LOGIN_BAN_DURATION - Duration::from_millis(500),
                || panic!("banned IP must not check passwords"),
            )
            .unwrap_err()
            .into_response();
        assert_eq!(retry.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(retry.headers()["retry-after"], "1");
        // Expiry resets the count, so the next failure is the first one again.
        assert_eq!(
            attempts
                .authenticate(ip, now + LOGIN_BAN_DURATION, || false)
                .unwrap_err()
                .status(),
            StatusCode::UNAUTHORIZED
        );
        attempts
            .authenticate(ip, now + LOGIN_BAN_DURATION, || true)
            .unwrap();
        assert!(attempts.0.is_empty());
    }

    #[test]
    fn successful_login_resets_only_its_own_ip_and_mapped_ipv6_shares_ipv4_count() {
        let now = Instant::now();
        let ip = "192.0.2.1".parse().unwrap();
        let other_ip = "2001:db8::1".parse().unwrap();
        let mut attempts = LoginAttempts::default();
        for _ in 0..2 {
            assert_eq!(
                attempts
                    .authenticate(ip, now, || false)
                    .unwrap_err()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
            assert_eq!(
                attempts
                    .authenticate(other_ip, now, || false)
                    .unwrap_err()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        attempts.authenticate(ip, now, || true).unwrap();
        for _ in 0..2 {
            assert_eq!(
                attempts
                    .authenticate(ip, now, || false)
                    .unwrap_err()
                    .status(),
                StatusCode::UNAUTHORIZED
            );
        }
        assert_eq!(
            attempts
                .authenticate("::ffff:192.0.2.1".parse().unwrap(), now, || false)
                .unwrap_err()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert_eq!(
            attempts
                .authenticate(other_ip, now, || false)
                .unwrap_err()
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        attempts
            .authenticate("192.0.2.2".parse().unwrap(), now, || true)
            .unwrap();
    }
}

#[cfg(test)]
mod route_tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
        routing::get,
        Json,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;

    #[tokio::test]
    async fn local_login_protects_routes_and_logout_clears_the_cookie() {
        config::init_test_config();
        let app = super::super::app().route("/session-scope", get(|session: Session| async move {
            Json(json!({"scope": session.storage_scope()}))
        }));
        let request = |method: &str, path: &str, cookie: Option<&str>, body: Value, origin: &str| {
            let mut builder = Request::builder().method(method).uri(path)
                .extension(ConnectInfo("192.0.2.10:12345".parse::<SocketAddr>().unwrap()))
                .header("origin", origin).header("content-type", "application/json");
            if let Some(cookie) = cookie { builder = builder.header("cookie", cookie); }
            builder.body(Body::from(body.to_string())).unwrap()
        };
        let origin = "http://localhost:3003";
        let missing = app.clone().oneshot(request("GET", "/api/assistant/ping", None, json!(null), origin)).await.unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
        let rejected = app.clone().oneshot(request("POST", "/auth/", None, json!({"password":"incorrect"}), origin)).await.unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
        assert!(!rejected.headers().contains_key("set-cookie"));
        let csrf = app.clone().oneshot(request("POST", "/auth/", None, json!({"password":config::get().auth_password}), "https://evil.example")).await.unwrap();
        assert_eq!(csrf.status(), StatusCode::FORBIDDEN);
        let account = app.clone().oneshot(request("POST", "/auth/", None, json!({"username":"owner","password":config::get().auth_password}), origin)).await.unwrap();
        assert_eq!(account.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let login = app.clone().oneshot(request("POST", "/auth/", None, json!({"password":config::get().auth_password}), origin)).await.unwrap();
        assert_eq!(login.status(), StatusCode::NO_CONTENT);
        let cookie = login.headers().get_all("set-cookie").iter().map(|v| v.to_str().unwrap())
            .find(|value| value.starts_with("firefly_session=")).unwrap();
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Lax"));
        assert!(cookie.contains("Path=/"));
        let cookie = cookie.split(';').next().unwrap().to_owned();
        let authenticated = app.clone().oneshot(request("GET", "/session-scope", Some(&cookie), json!(null), origin)).await.unwrap();
        assert_eq!(authenticated.status(), StatusCode::OK);
        let body = to_bytes(authenticated.into_body(), 4096).await.unwrap();
        assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), json!({"scope":db::SINGLE_SCOPE}));
        let jwt = jwt::validate(cookie.strip_prefix("firefly_session=").unwrap()).unwrap();
        assert!(serde_json::to_value(jwt).unwrap().get("sub").is_none());
        let foreign_logout = app.clone().oneshot(request("POST", "/auth/logout", Some(&cookie), json!(null), "https://evil.example")).await.unwrap();
        assert_eq!(foreign_logout.status(), StatusCode::FORBIDDEN);
        let logout = app.clone().oneshot(request("POST", "/auth/logout", Some(&cookie), json!(null), origin)).await.unwrap();
        assert_eq!(logout.status(), StatusCode::NO_CONTENT);
        assert!(logout.headers().get("set-cookie").unwrap().to_str().unwrap().starts_with("firefly_session=;"));
        let logged_out = app.clone().oneshot(request("GET", "/api/assistant/ping", None, json!(null), origin)).await.unwrap();
        assert_eq!(logged_out.status(), StatusCode::UNAUTHORIZED);
        let old_callback = app.oneshot(request("GET", "/auth/callback", None, json!(null), origin)).await.unwrap();
        assert_eq!(old_callback.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn blacklist_applies_to_both_login_routes_and_ignores_spoofed_ip_headers() {
        config::init_test_config();
        let app = super::super::app();
        let request = |ip: &str, port: u16, path: &str, password: &str, forwarded: &str| {
            Request::builder()
                .method("POST")
                .uri(path)
                .extension(ConnectInfo(SocketAddr::new(ip.parse().unwrap(), port)))
                .header("origin", "http://localhost:3003")
                .header("content-type", "application/json")
                .header("x-forwarded-for", forwarded)
                .header("x-real-ip", forwarded)
                .body(Body::from(json!({"password": password}).to_string()))
                .unwrap()
        };
        for (index, path) in ["/auth", "/auth/", "/auth"].iter().enumerate() {
            let response = app
                .clone()
                .oneshot(request(
                    "192.0.2.20",
                    12000 + index as u16,
                    path,
                    "incorrect",
                    "198.51.100.1",
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                if index == 2 {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::UNAUTHORIZED
                }
            );
            assert!(!response.headers().contains_key("set-cookie"));
            if index == 2 {
                assert_eq!(response.headers()["retry-after"], "3600");
            }
        }
        let blocked = app
            .clone()
            .oneshot(request(
                "192.0.2.20",
                13000,
                "/auth/",
                &config::get().auth_password,
                "198.51.100.2",
            ))
            .await
            .unwrap();
        assert_eq!(blocked.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(!blocked.headers().contains_key("set-cookie"));
        let other = app
            .oneshot(request(
                "192.0.2.21",
                13000,
                "/auth/",
                &config::get().auth_password,
                "198.51.100.1",
            ))
            .await
            .unwrap();
        assert_eq!(other.status(), StatusCode::NO_CONTENT);
    }
}

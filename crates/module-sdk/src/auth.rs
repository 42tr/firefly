use axum::{extract::FromRequestParts, http::request::Parts};
#[derive(Clone, Copy)]
pub struct Session;
impl Session {
    pub fn storage_scope(&self) -> &'static str {
        "1"
    }
}
#[derive(Clone, Copy)]
pub struct RequireAuth;
impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(_: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self)
    }
}
impl<S: Send + Sync> FromRequestParts<S> for RequireAuth {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(_: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self)
    }
}

extern crate regex;

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use async_recursion::async_recursion;
use derive_builder::Builder;
use regex::Regex;
use reqwest::browser::ChromeVersion;
use reqwest::header::{HeaderMap, HeaderValue, self};
use reqwest::redirect::Policy;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use base64::{engine::general_purpose, Engine as _};
use rand::Rng;
use reqwest::{Client, Proxy, StatusCode, Url};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::{debug, AuthError, AuthResult, URL_CHATGPT_API};

const CLIENT_ID: &str = "pdlLIX2Y72MIl2rhLhTE9VV9bN905kBh";
const OPENAI_OAUTH_URL: &str = "https://auth0.openai.com";
const OPENAI_OAUTH_TOKEN_URL: &str = "https://auth0.openai.com/oauth/token";
const OPENAI_OAUTH_REVOKE_URL: &str = "https://auth0.openai.com/oauth/revoke";
const OPENAI_OAUTH_CALLBACK_URL: &str =
    "com.openai.chat://auth0.openai.com/ios/com.openai.chat/callback";

const OPENAI_API_URL: &str = "https://api.openai.com";

pub enum AuthStrategy {
    IOS,
    PLATFROM,
}

#[async_trait::async_trait]
pub trait AuthHandle: Send + Sync {
    async fn do_access_token(&self, account: &AuthAccount) -> AuthResult<AccessToken>;
}
/// You do **not** have to wrap the `Client` in an [`Rc`] or [`Arc`] to **reuse** it,
/// because it already uses an [`Arc`] internally.
///
/// [`Rc`]: std::rc::Rc
#[derive(Clone)]
pub struct AuthClient {
    client: Client,
    email_regex: Regex,
    handle: Arc<Box<dyn AuthHandle + Send + Sync>>,
}

#[async_trait::async_trait]
impl AuthHandle for AuthClient {
    async fn do_access_token(&self, account: &AuthAccount) -> AuthResult<AccessToken> {
        if !self.email_regex.is_match(&account.username) || account.password.is_empty() {
            bail!(AuthError::InvalidEmailOrPassword)
        }
        self.handle.do_access_token(account).await
    }
}

impl AuthClient {
    pub async fn do_dashboard_login(&self, access_token: &str) -> AuthResult<DashSession> {
        let access_token = access_token.replace("Bearer ", "");
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/onboarding/login"))
            .bearer_auth(access_token)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        Self::response_handle(resp).await
    }

    pub async fn do_get_api_key(&self, sensitive_id: &str, name: &str) -> AuthResult<ApiKey> {
        let data = ApiKeyDataBuilder::default()
            .action("create")
            .name(name)
            .build()?;
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        Self::response_handle(resp).await
    }

    pub async fn do_get_api_key_list(&self, sensitive_id: &str) -> AuthResult<ApiKeyList> {
        let resp = self
            .client
            .get(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        Self::response_handle(resp).await
    }

    pub async fn do_delete_api_key(
        &self,
        sensitive_id: &str,
        redacted_key: &str,
        created_at: u64,
    ) -> AuthResult<ApiKey> {
        let data = ApiKeyDataBuilder::default()
            .action("delete")
            .redacted_key(redacted_key)
            .created_at(created_at)
            .build()?;
        let resp = self
            .client
            .post(format!("{OPENAI_API_URL}/dashboard/user/api_keys"))
            .bearer_auth(sensitive_id)
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        Self::response_handle(resp).await
    }

    pub async fn do_refresh_token(&self, refresh_token: &str) -> AuthResult<RefreshToken> {
        let refresh_token = Self::verify_refresh_token(refresh_token)?;
        let data = RefreshTokenDataBuilder::default()
            .redirect_uri(OPENAI_OAUTH_CALLBACK_URL)
            .grant_type(GrantType::RefreshToken)
            .client_id(CLIENT_ID)
            .refresh_token(refresh_token)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_TOKEN_URL)
            .json(&data)
            .send()
            .await?;

        let mut token = Self::response_handle::<RefreshToken>(resp).await?;
        token.refresh_token = refresh_token.to_owned();
        Ok(token)
    }

    pub async fn do_revoke_token(&self, refresh_token: &str) -> AuthResult<()> {
        let refresh_token = Self::verify_refresh_token(refresh_token)?;
        let data = RevokeTokenDataBuilder::default()
            .client_id(CLIENT_ID)
            .token(refresh_token)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_REVOKE_URL)
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        Self::response_handle_unit(resp).await
    }

    async fn response_handle<U: DeserializeOwned>(resp: reqwest::Response) -> AuthResult<U> {
        let url = resp.url().clone();
        match resp.error_for_status_ref() {
            Ok(_) => Ok(resp
                .json::<U>()
                .await
                .map_err(|op| AuthError::DeserializeError(op.to_string()))?),
            Err(err) => {
                let err_msg = format!("error: {}, url: {}", resp.text().await?, url);
                bail!(Self::handle_error(err.status(), err_msg).await)
            }
        }
    }

    async fn response_handle_unit(resp: reqwest::Response) -> AuthResult<()> {
        let url = resp.url().clone();

        match resp.error_for_status_ref() {
            Ok(_) => Ok(()),
            Err(err) => {
                let err_msg = format!("error: {}, url: {}", resp.text().await?, url);
                bail!(Self::handle_error(err.status(), err_msg).await)
            }
        }
    }

    async fn handle_error(status: Option<StatusCode>, err_msg: String) -> AuthError {
        match status {
            Some(
                status_code @ (StatusCode::UNAUTHORIZED
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
                | StatusCode::BAD_REQUEST
                | StatusCode::PAYMENT_REQUIRED
                | StatusCode::FORBIDDEN
                | StatusCode::INTERNAL_SERVER_ERROR
                | StatusCode::BAD_GATEWAY
                | StatusCode::SERVICE_UNAVAILABLE
                | StatusCode::GATEWAY_TIMEOUT),
            ) => {
                if status_code == StatusCode::UNAUTHORIZED {
                    return AuthError::Unauthorized("Unauthorized".to_owned());
                }
                if status_code == StatusCode::TOO_MANY_REQUESTS {
                    return AuthError::TooManyRequests("Too Many Requests".to_owned());
                }
                if status_code == StatusCode::BAD_REQUEST {
                    return AuthError::BadRequest("Bad Request".to_owned());
                }

                if status_code.is_client_error() {
                    return AuthError::InvalidClientRequest(err_msg);
                }

                AuthError::ServerError(err_msg)
            }
            _ => AuthError::InvalidRequest("Invalid Request".to_owned()),
        }
    }

    fn generate_code_verifier() -> String {
        let token: [u8; 32] = rand::thread_rng().gen();
        let code_verifier = general_purpose::URL_SAFE
            .encode(token)
            .trim_end_matches('=')
            .to_string();
        code_verifier
    }

    fn generate_code_challenge(code_verifier: &str) -> String {
        let mut m = Sha256::new();
        m.update(code_verifier.as_bytes());
        let code_challenge = general_purpose::URL_SAFE
            .encode(m.finalize())
            .trim_end_matches('=')
            .to_string();
        code_challenge
    }

    fn get_callback_code(url: &Url) -> AuthResult<String> {
        let mut url_params = HashMap::new();
        url.query_pairs().into_owned().for_each(|(key, value)| {
            url_params
                .entry(key)
                .and_modify(|v: &mut Vec<String>| v.push(value.clone()))
                .or_insert(vec![value]);
        });

        debug!("get_callback_code: {:?}", url_params);

        if let Some(error) = url_params.get("error") {
            if let Some(error_description) = url_params.get("error_description") {
                let msg = format!("{}: {}", error[0], error_description[0]);
                bail!("{}", msg)
            } else {
                bail!("{}", error[0])
            }
        }

        let code = url_params
            .get("code")
            .ok_or(AuthError::FailedCallbackCode)?[0]
            .to_string();
        Ok(code)
    }

    fn get_callback_state(url: &Url) -> String {
        let url_params = url.query_pairs().into_owned().collect::<HashMap<_, _>>();
        debug!("get_callback_state: {:?}", url_params);
        url_params["state"].to_owned()
    }

    fn get_location_path(header: &HeaderMap<HeaderValue>) -> AuthResult<&str> {
        debug!("get_location_path: {:?}", header);
        Ok(header
            .get("Location")
            .ok_or(AuthError::InvalidLocation)?
            .to_str()?)
    }

    fn verify_refresh_token(t: &str) -> AuthResult<&str> {
        let refresh_token = t.trim_start_matches("Bearer ");
        if refresh_token.is_empty() {
            bail!(AuthError::InvalidRefreshToken)
        }
        Ok(refresh_token)
    }
}

struct PlatformAuthHandle {
    client: Client,
}

#[async_trait::async_trait]
impl AuthHandle for PlatformAuthHandle {
    async fn do_access_token(&self, account: &AuthAccount) -> AuthResult<AccessToken> {
        let csrf_token = self.get_csrf_token().await?;

        let authorized_url = self.get_authorized_url(csrf_token).await?;

        let state = AuthClient::get_callback_state(&authorized_url);

        // check username
        self.authenticate_username(&state, &account.username)
            .await?;

        // check password and username
        self.authenticate_password(&state, &account).await?;
        todo!()
    }
}

impl PlatformAuthHandle {
    async fn get_authorized_url(&self, csrf_token: String) -> AuthResult<Url> {
        debug!("csrf_token: {csrf_token}");
        let form = [("callbackUrl", "/"), ("csrfToken", &csrf_token), ("json", "true")];
        let resp = self
            .client
            .post(format!(
                "{URL_CHATGPT_API}/api/auth/signin/auth0?prompt=login"
            ))
            .form(&form)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        println!("{}", resp.status());

        if resp.status().is_success() {
            let res = resp.json::<Value>().await?;
            // let url = res
            //     .as_object()
            //     .context(AuthError::FailedAuthorizedUrl)?
            //     .get("url")
            //     .context(AuthError::FailedAuthorizedUrl)?;
            println!("url: {}", res);
        }

        bail!(AuthError::FailedAuthorizedUrl)
    }

    async fn get_csrf_token(&self) -> AuthResult<String> {
        let resp = self
            .client
            .get(format!("{URL_CHATGPT_API}/api/auth/csrf"))
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        if resp.status().is_success() {
            let res = resp.json::<Value>().await?;
            let csrf_token = res
                .as_object()
                .context(AuthError::FailedCsrfToken)?
                .get("csrfToken")
                .context(AuthError::FailedCsrfToken)?;
            return Ok(csrf_token.to_string());
        }
        bail!(AuthError::FailedCsrfToken)
    }

    async fn authenticate_username(&self, state: &str, username: &str) -> AuthResult<()> {
        let url = format!("{OPENAI_OAUTH_URL}/u/login/identifier?state={state}");
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .json(
                &IdentifierDataBuilder::default()
                    .action("default")
                    .state(&state)
                    .username(username)
                    .js_available(true)
                    .webauthn_available(true)
                    .is_brave(false)
                    .webauthn_platform_available(false)
                    .build()?,
            )
            .send()
            .await?;

        AuthClient::response_handle_unit(resp)
            .await
            .context(AuthError::InvalidEmail)
    }

    async fn authenticate_password(
        &self,
        state: &str,
        account: &AuthAccount,
    ) -> AuthResult<AccessToken> {
        let data = AuthenticateDataBuilder::default()
            .action("default")
            .state(state)
            .username(&account.username)
            .password(&account.password)
            .build()?;

        let url = format!("{OPENAI_OAUTH_URL}/u/login/password?state={state}");

        let resp = self
            .client
            .post(&url)
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        let headers = resp.headers().clone();
        let status = resp.status();

        AuthClient::response_handle_unit(resp)
            .await
            .map_err(|_| AuthError::InvalidEmailOrPassword)?;

        if status.is_redirection() {
            let location = AuthClient::get_location_path(&headers)?;
            let resp = self
                .client
                .get(format!("{OPENAI_OAUTH_URL}{location}"))
                .send()
                .await
                .map_err(AuthError::FailedRequest)?;
            if resp.status().is_redirection() {
                let location = AuthClient::get_location_path(resp.headers())?;
                if location.starts_with("/u/mfa-otp-challenge") {
                    let mfa = account.mfa.clone().ok_or(AuthError::MFARequired)?;
                    return self.authenticate_mfa(&mfa, location, &account).await;
                }
                let resp = self
                    .client
                    .get(location)
                    .send()
                    .await
                    .map_err(AuthError::FailedRequest)?;

                return match resp.status() {
                    StatusCode::FOUND => self.get_access_token().await,
                    StatusCode::TEMPORARY_REDIRECT => {
                        bail!(AuthError::InvalidEmailOrPassword)
                    }
                    _ => {
                        bail!("Failed to get access token")
                    }
                };
            }
        }
        bail!(AuthError::FailedLogin)
    }

    async fn authenticate_mfa(
        &self,
        mfa_code: &str,
        location: &str,
        account: &AuthAccount,
    ) -> AuthResult<AccessToken> {
        let url = format!("{OPENAI_OAUTH_URL}{}", location);
        let state = AuthClient::get_callback_state(&Url::parse(&url)?);
        let data = AuthenticateMfaDataBuilder::default()
            .action("default")
            .state(&state)
            .code(mfa_code)
            .build()?;

        let resp = self
            .client
            .post(&url)
            .json(&data)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        let headers = resp.headers().clone();

        AuthClient::response_handle_unit(resp).await?;

        let location: &str = AuthClient::get_location_path(&headers)?;
        if location.starts_with("/authorize/resume?") && account.mfa.is_none() {
            bail!(AuthError::MFAFailed)
        }
        // self.authenticate_resume(location, &url, &account)
        //     .await
        todo!()
    }

    async fn get_access_token(&self) -> AuthResult<AccessToken> {
        let resp = self
            .client
            .get("https://chat.openai.com/api/auth/session")
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        match resp.status() {
            StatusCode::OK => Ok(resp.json::<AccessToken>().await?),
            StatusCode::TOO_MANY_REQUESTS => {
                bail!(AuthError::TooManyRequests("Too Many Requests".to_owned()))
            }
            _ => {
                bail!("Failed to get access token")
            }
        }
    }
}

struct IosAuthHandle {
    client: Client,
}

#[async_trait::async_trait]
impl AuthHandle for IosAuthHandle {
    async fn do_access_token(&self, account: &AuthAccount) -> AuthResult<AccessToken> {
        let code_verifier = AuthClient::generate_code_verifier();
        let code_challenge = AuthClient::generate_code_challenge(&code_verifier);

        let url = self
            .get_authorized_url(&code_challenge)
            .await?;
        let state = AuthClient::get_callback_state(&url);

        // check username
        self.authenticate_username(&state, account).await?;

        // check password and username
        self.authenticate_password(&code_verifier, &state, account)
            .await
    }
}

impl IosAuthHandle {
    async fn get_authorized_url(
        &self,
        code_challenge: &str,
    ) -> AuthResult<Url> {
        let preauth_cookie = self.unofficial_preauth_cookie().await?;
        let url = format!("https://auth0.openai.com/authorize?state=4DJBNv86mezKHDv-i2wMuDBea2-rHAo5nA_ZT4zJeak&ios_app_version=1744&client_id={CLIENT_ID}&redirect_uri={OPENAI_OAUTH_CALLBACK_URL}&code_challenge={code_challenge}&scope=openid%20email%20profile%20offline_access%20model.request%20model.read%20organization.read%20organization.write&prompt=login&preauth_cookie={preauth_cookie}&audience=https://api.openai.com/v1&code_challenge_method=S256&response_type=code&auth0Client=eyJ2ZXJzaW9uIjoiMi4zLjIiLCJuYW1lIjoiQXV0aDAuc3dpZnQiLCJlbnYiOnsic3dpZnQiOiI1LngiLCJpT1MiOiIxNi4yIn19");
        let resp = self
            .client
            .get(&url)
            .header(
                reqwest::header::REFERER,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        let url = resp.url().clone();

        AuthClient::response_handle_unit(resp)
            .await
            .map_err(|e| AuthError::InvalidLoginUrl(e.to_string()))?;

        Ok(url.clone())
    }

    async fn authenticate_username(&self, state: &str, account: &AuthAccount) -> AuthResult<()> {
        let url = format!("{OPENAI_OAUTH_URL}/u/login/identifier?state={state}");
        let resp = self
            .client
            .post(&url)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .json(
                &IdentifierDataBuilder::default()
                    .action("default")
                    .state(&state)
                    .username(&account.username)
                    .js_available(true)
                    .webauthn_available(true)
                    .is_brave(false)
                    .webauthn_platform_available(false)
                    .build()?,
            )
            .send()
            .await?;

        AuthClient::response_handle_unit(resp)
            .await
            .context(AuthError::InvalidEmail)
    }

    async fn authenticate_password(
        &self,
        code_verifier: &str,
        state: &str,
        account: &AuthAccount,
    ) -> AuthResult<AccessToken> {
        debug!("authenticate_password state: {state}");
        let data = AuthenticateDataBuilder::default()
            .action("default")
            .state(state)
            .username(&account.username)
            .password(&account.password)
            .build()?;

        let resp = self
            .client
            .post(format!("{OPENAI_OAUTH_URL}/u/login/password?state={state}"))
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        let headers = resp.headers().clone();
        AuthClient::response_handle_unit(resp)
            .await
            .map_err(|_| AuthError::InvalidEmailOrPassword)?;

        let location = AuthClient::get_location_path(&headers)?;
        debug!("authenticate_password location path: {location}");
        if location.starts_with("/authorize/resume?") {
            return self
                .authenticate_resume(code_verifier, location, account)
                .await;
        }
        bail!(AuthError::FailedLogin)
    }

    async fn authenticate_resume(
        &self,
        code_verifier: &str,
        location: &str,
        account: &AuthAccount,
    ) -> AuthResult<AccessToken> {
        let resp = self
            .client
            .get(&format!("{OPENAI_OAUTH_URL}{location}"))
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        let headers = resp.headers().clone();

        AuthClient::response_handle_unit(resp)
            .await
            .map_err(|_| AuthError::InvalidLocation)?;

        let location: &str = AuthClient::get_location_path(&headers)?;
        debug!("authenticate_resume location path: {location}");
        if location.starts_with("/u/mfa-otp-challenge?") {
            let mfa = account.mfa.clone().ok_or(AuthError::MFARequired)?;
            self.authenticate_mfa(&mfa, code_verifier, location, account)
                .await
        } else if !location.starts_with(OPENAI_OAUTH_CALLBACK_URL) {
            bail!(AuthError::FailedCallbackURL)
        } else {
            self.authorization_code(code_verifier, location).await
        }
    }

    #[async_recursion]
    async fn authenticate_mfa(
        &self,
        mfa_code: &str,
        code_verifier: &str,
        location: &str,
        account: &AuthAccount,
    ) -> AuthResult<AccessToken> {
        let url = format!("{OPENAI_OAUTH_URL}{}", location);
        let state = AuthClient::get_callback_state(&Url::parse(&url)?);
        let data = AuthenticateMfaDataBuilder::default()
            .action("default")
            .state(&state)
            .code(mfa_code)
            .build()?;

        let resp = self
            .client
            .post(&url)
            .json(&data)
            .header(reqwest::header::REFERER, HeaderValue::from_str(&url)?)
            .header(
                reqwest::header::ORIGIN,
                HeaderValue::from_static(OPENAI_OAUTH_URL),
            )
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;
        let headers = resp.headers().clone();

        AuthClient::response_handle_unit(resp).await?;

        let location: &str = AuthClient::get_location_path(&headers)?;
        if location.starts_with("/authorize/resume?") && account.mfa.is_none() {
            bail!(AuthError::MFAFailed)
        }
        self.authenticate_resume(code_verifier, location, &account)
            .await
    }

    async fn authorization_code(
        &self,
        code_verifier: &str,
        location: &str,
    ) -> AuthResult<AccessToken> {
        debug!("authorization_code location path: {location}");
        let code = AuthClient::get_callback_code(&Url::parse(location)?)?;
        let data = AuthorizationCodeDataBuilder::default()
            .redirect_uri(OPENAI_OAUTH_CALLBACK_URL)
            .grant_type(GrantType::AuthorizationCode)
            .client_id(CLIENT_ID)
            .code(&code)
            .code_verifier(code_verifier)
            .build()?;

        let resp = self
            .client
            .post(OPENAI_OAUTH_TOKEN_URL)
            .json(&data)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        AuthClient::response_handle::<AccessToken>(resp).await
    }

    /// It may fail at any time
    #[allow(dead_code)]
    async fn official_preauth_cookie(&self) -> AuthResult<String> {
        let mut kv = HashMap::new();
        kv.insert("bundle_id", "com.openai.chat");
        kv.insert("device_id", "0E92DAF9-94F0-4F77-BDF4-53A60D19EC65");
        kv.insert("request_flag", "true");
        kv.insert("device_token", "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");

        let resp = self
            .client
            .post("https://ios.chat.openai.com/backend-api/preauth_devicecheck")
            .json(&kv)
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        if resp.status().is_success() {
            if let Some(preauth_cookie) = resp
                .cookies()
                .into_iter()
                .find(|c| c.name().eq("_preauth_devicecheck"))
            {
                return Ok(preauth_cookie.value().to_owned());
            }
        }

        bail!(AuthError::BadRequest(
            "Failed to get preauth_devicecheck".to_owned()
        ))
    }

    #[allow(dead_code)]
    async fn unofficial_preauth_cookie(&self) -> AuthResult<String> {
        let resp = self
            .client
            .get("https://ai.fakeopen.com/auth/preauth")
            .send()
            .await
            .map_err(AuthError::FailedRequest)?;

        if resp.status().is_success() {
            let json = resp.json::<serde_json::Value>().await?;

            if let Some(kv) = json.as_object() {
                if let Some(preauth_cookie) = kv.get("preauth_cookie") {
                    return Ok(preauth_cookie
                        .as_str()
                        .expect("failed to extract preauth_cookie")
                        .to_owned());
                }
            }
        }

        bail!(AuthError::FailedLogin)
    }
}

#[derive(Serialize, Builder)]
pub struct ApiKeyData<'a> {
    action: &'a str,
    #[builder(setter(into, strip_option), default)]
    name: Option<&'a str>,
    #[builder(setter(into, strip_option), default)]
    redacted_key: Option<&'a str>,
    #[builder(setter(into, strip_option), default)]
    created_at: Option<u64>,
}

#[derive(Serialize, Builder)]
struct IdentifierData<'a> {
    state: &'a str,
    username: &'a str,
    #[serde(rename = "js-available")]
    js_available: bool,
    #[serde(rename = "webauthn-available")]
    webauthn_available: bool,
    #[serde(rename = "is-brave")]
    is_brave: bool,
    #[serde(rename = "webauthn-platform-available")]
    webauthn_platform_available: bool,
    action: &'a str,
}

#[derive(Clone)]
enum GrantType {
    AuthorizationCode,
    RefreshToken,
}

impl Serialize for GrantType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            GrantType::AuthorizationCode => serializer.serialize_str("authorization_code"),
            GrantType::RefreshToken => serializer.serialize_str("refresh_token"),
        }
    }
}

#[derive(Serialize, Builder)]
struct AuthenticateData<'a> {
    state: &'a str,
    username: &'a str,
    password: &'a str,
    action: &'a str,
}

#[derive(Serialize, Builder)]
struct AuthenticateMfaData<'a> {
    state: &'a str,
    code: &'a str,
    action: &'a str,
}

#[derive(Serialize, Builder)]
struct AuthorizationCodeData<'a> {
    redirect_uri: &'a str,
    grant_type: GrantType,
    client_id: &'a str,
    code_verifier: &'a str,
    code: &'a str,
}

#[derive(Serialize, Builder)]
struct RevokeTokenData<'a> {
    client_id: &'a str,
    token: &'a str,
}

#[derive(Serialize, Builder)]
struct RefreshTokenData<'a> {
    redirect_uri: &'a str,
    grant_type: GrantType,
    client_id: &'a str,
    refresh_token: &'a str,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AccessToken {
    pub access_token: String,
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
    pub puid: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct RefreshToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
    pub id_token: String,
    pub expires_in: i64,
}

#[derive(Deserialize, Builder)]
pub struct AuthAccount {
    username: String,
    password: String,
    #[builder(setter(into, strip_option), default)]
    mfa: Option<String>,
}

impl AuthAccount {
    pub fn username(&self) -> &str {
        self.username.as_ref()
    }

    pub fn password(&self) -> &str {
        self.password.as_ref()
    }

    pub fn mfa(&self) -> Option<&str> {
        self.mfa.as_deref()
    }
}

#[derive(Debug, Deserialize)]
pub struct DashSession {
    pub object: String,
    pub user: User,
    pub invites: Vec<Value>,
}

impl DashSession {
    pub fn sensitive_id(&self) -> &str {
        &self.user.session.sensitive_id
    }

    pub fn user_id(&self) -> &str {
        &self.user.id
    }

    pub fn nickname(&self) -> &str {
        &self.user.name
    }

    pub fn email(&self) -> &str {
        &self.user.email
    }

    pub fn picture(&self) -> &str {
        &self.user.picture
    }
}

#[derive(Debug, Deserialize)]
pub struct User {
    pub object: String,
    pub id: String,
    pub email: String,
    pub name: String,
    pub picture: String,
    pub created: i64,
    pub groups: Vec<Value>,
    pub session: Session,
    pub orgs: Orgs,
    pub intercom_hash: String,
    pub amr: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct Session {
    pub sensitive_id: String,
    pub object: String,
    pub name: Option<String>,
    pub created: i64,
    pub last_use: Option<i64>,
    pub publishable: bool,
}

#[derive(Debug, Deserialize)]
pub struct Orgs {
    pub object: String,
    pub data: Vec<OrgsData>,
}

#[derive(Debug, Deserialize)]
pub struct OrgsData {
    pub object: String,
    pub id: String,
    pub created: i64,
    pub title: String,
    pub name: String,
    pub description: String,
    pub personal: bool,
    pub is_default: bool,
    pub role: String,
    pub groups: Vec<Value>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKey {
    pub result: String,
    pub key: Option<Key>,
}

#[derive(Debug, Deserialize)]
pub struct ApiKeyList {
    pub object: String,
    pub data: Vec<Key>,
}

#[derive(Debug, Deserialize)]
pub struct Key {
    #[serde(rename = "sensitive_id")]
    pub sensitive_id: String,
    pub object: String,
    pub name: String,
    pub created: i64,
    #[serde(rename = "last_use")]
    pub last_use: Value,
    pub publishable: bool,
}

pub struct AuthClientBuilder {
    builder: reqwest::ClientBuilder,
    strategy: AuthStrategy,
}

impl AuthClientBuilder {
    // Proxy options
    pub fn proxy(mut self, proxy: Option<String>) -> Self {
        if let Some(url) = proxy {
            self.builder = self.builder.proxy(
                Proxy::all(url.clone()).expect(&format!("reqwest: invalid proxy url: {url}")),
            );
        } else {
            self.builder = self.builder.no_proxy();
        }
        self
    }

    // Timeout options

    /// Enables a request timeout.
    ///
    /// The timeout is applied from when the request starts connecting until the
    /// response body has finished.
    ///
    /// Default is no timeout.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.timeout(timeout);
        self
    }

    /// Set a timeout for only the connect phase of a `Client`.
    ///
    /// Default is `None`.
    ///
    /// # Note
    ///
    /// This **requires** the futures be executed in a tokio runtime with
    /// a tokio timer enabled.
    pub fn connect_timeout(mut self, timeout: Duration) -> Self {
        self.builder = self.builder.connect_timeout(timeout);
        self
    }

    // HTTP options

    /// Set an optional timeout for idle sockets being kept-alive.
    ///
    /// Pass `None` to disable timeout.
    ///
    /// Default is 90 seconds.
    pub fn pool_idle_timeout<D>(mut self, val: D) -> Self
    where
        D: Into<Option<Duration>>,
    {
        self.builder = self.builder.pool_idle_timeout(val);
        self
    }

    /// Sets the maximum idle connection per host allowed in the pool.
    pub fn pool_max_idle_per_host(mut self, max: usize) -> Self {
        self.builder = self.builder.pool_max_idle_per_host(max);
        self
    }

    /// Enable a persistent cookie store for the client.
    ///
    /// Cookies received in responses will be preserved and included in
    /// additional requests.
    ///
    /// By default, no cookie store is used.
    ///
    /// # Optional
    ///
    /// This requires the optional `cookies` feature to be enabled.
    pub fn cookie_store(mut self, store: bool) -> Self {
        self.builder = self.builder.cookie_store(store);
        self
    }

    /// Set that all sockets have `SO_KEEPALIVE` set with the supplied duration.
    ///
    /// If `None`, the option will not be set.
    pub fn tcp_keepalive<D>(mut self, val: D) -> Self
    where
        D: Into<Option<Duration>>,
    {
        self.builder = self.builder.tcp_keepalive(val);
        self
    }

    /// Sets the necessary values to mimic the specified Chrome version.
    pub fn chrome_builder(mut self, ver: ChromeVersion) -> Self {
        self.builder = self.builder.chrome_builder(ver);
        self
    }

    /// Sets the `User-Agent` header to be used by this client.
    pub fn user_agent(mut self, value: &str) -> Self {
        self.builder = self.builder.user_agent(value);
        self
    }

    /// Handle auth strategy, default is `AuthStrategy::IOS`
    pub fn handle(mut self, strategy: AuthStrategy) -> Self {
        self.strategy = strategy;
        self
    }

    pub fn build(self) -> AuthClient {
        let client = self.builder.build().expect("ClientBuilder::build()");

        let handle: Box<dyn AuthHandle + Send + Sync> = match self.strategy {
            AuthStrategy::IOS => {
                let handle = IosAuthHandle {
                    client: client.clone(),
                };
                Box::new(handle)
            }
            AuthStrategy::PLATFROM => {
                let handle = PlatformAuthHandle {
                    client: client.clone(),
                };
                Box::new(handle)
            }
        };

        AuthClient {
            client,
            email_regex: Regex::new(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Z|a-z]{2,7}\b")
                .expect("Regex::new()"),
            handle: Arc::new(handle),
        }
    }

    pub fn builder() -> AuthClientBuilder {
        let client_builder = Client::builder().redirect(Policy::custom(|attempt| {
            if attempt
                .url()
                .to_string()
                .contains("https://auth0.openai.com/u/login/identifier")
            {
                // redirects to 'https://auth0.openai.com/u/login/identifier'
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));
        AuthClientBuilder {
            builder: client_builder,
            strategy: AuthStrategy::PLATFROM,
        }
    }
}

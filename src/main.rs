mod condvar_rwl;
mod logger_type;
mod room;
mod room_list;
mod session;
mod sync_record;

use condvar_rwl::CondvarRwl;
use logger_type::Logger;
use room::{room_t, user_t};
use sync_record::{event_t, phase_t, sync_record_t};

use axum::routing::{get as get_method, post as post_method};
use std::*;

type Request = axum::extract::Request;
type Response = axum::response::Response;
type Json = serde_json::Value;

macro_rules! lazy {
    ($(static $name:ident : $typ:ty = $init:expr;)+) => {
        $(static $name : ::std::sync::LazyLock<$typ> = ::std::sync::LazyLock::new(|| $init);)+
    };
}
macro_rules! clone_capture {
    ([$($variables:ident),+] $closure:expr) => {
        {
            $(let $variables = $variables.clone();)+
            $closure
        }
    };
}
fn stoul(v: String) -> u64 {
    v.parse().unwrap()
}

mod dummy_at_method {
    pub trait Dummy {
        fn at(&self, p: &str) -> Result<serde_json::Value, crate::AgError>;
    }
    impl Dummy for serde_json::Value {
        fn at(&self, p: &str) -> Result<serde_json::Value, crate::AgError> {
            <&serde_json::Value>::at(&self, p)
        }
    }
    impl Dummy for &serde_json::Value {
        fn at(&self, p: &str) -> Result<serde_json::Value, crate::AgError> {
            self.get(p).cloned().ok_or_else(|| {
                crate::AgError::bad_request_error(format!(
                    "The JSON's property '{}' is not found",
                    p
                ))
            })
        }
    }
}
use dummy_at_method::Dummy;

/// Get the value of an environment variable or a default value.
fn getenv_or(key: &str, default_value: &str) -> String {
    env::var(key).unwrap_or_else(|_| default_value.to_owned())
}

// constants

const API_VER: i32 = 2;
lazy! {
    static API_PATH: String = format!("/v{}", API_VER);
    static PORT: u16 = getenv_or("PORT", "7468").parse().unwrap();
    static PASSWORD: String = getenv_or("PASSWORD", "");
    static ROOM_LIMIT: i32 = getenv_or("ROOM_LIMIT", "100").parse().unwrap();
    static LOBBY_LIFETIME: time::Duration = time::Duration::from_secs(60 * stoul(getenv_or("LOBBY_LIFETIME", "10")));
    static GAME_LIFETIME: time::Duration = time::Duration::from_secs(60 * stoul(getenv_or("GAME_LIFETIME", "20")));
}
const EXPIRE_TIMEOUT: time::Duration = time::Duration::from_secs(10);
const CLEANER_INTERVAL: time::Duration = time::Duration::from_secs(3);

fn log_stdout(msg: &str) {
    eprintln!("{}", msg)
}
fn log_stderr(msg: &str) {
    println!("{}", msg)
}

// Error

#[derive(Debug)]
pub enum AgError {
    SerdeJsonError(serde_json::Error),
    RmpEncodeError(rmp_serde::encode::Error),
    RmpDecodeError(rmp_serde::decode::Error),
    ErrorWithHttpStatus(axum::http::StatusCode, borrow::Cow<'static, str>),
}

impl AgError {
    pub fn not_found_error(msg: impl Into<borrow::Cow<'static, str>>) -> Self {
        Self::ErrorWithHttpStatus(axum::http::StatusCode::NOT_FOUND, msg.into())
    }
    pub fn too_many_requests_error(msg: impl Into<borrow::Cow<'static, str>>) -> Self {
        Self::ErrorWithHttpStatus(axum::http::StatusCode::TOO_MANY_REQUESTS, msg.into())
    }
    pub fn forbidden_error(msg: impl Into<borrow::Cow<'static, str>>) -> Self {
        Self::ErrorWithHttpStatus(axum::http::StatusCode::FORBIDDEN, msg.into())
    }
    pub fn bad_request_error(msg: impl Into<borrow::Cow<'static, str>>) -> Self {
        Self::ErrorWithHttpStatus(axum::http::StatusCode::BAD_REQUEST, msg.into())
    }
    pub fn unauthorized_error(msg: impl Into<borrow::Cow<'static, str>>) -> Self {
        Self::ErrorWithHttpStatus(axum::http::StatusCode::UNAUTHORIZED, msg.into())
    }
}

impl From<serde_json::Error> for AgError {
    fn from(value: serde_json::Error) -> Self {
        Self::SerdeJsonError(value)
    }
}

impl From<rmp_serde::encode::Error> for AgError {
    fn from(value: rmp_serde::encode::Error) -> Self {
        Self::RmpEncodeError(value)
    }
}

impl From<rmp_serde::decode::Error> for AgError {
    fn from(value: rmp_serde::decode::Error) -> Self {
        Self::RmpDecodeError(value)
    }
}

// UUID

fn uuid_from_json_value(val: serde_json::Value) -> Result<uuid::Uuid, AgError> {
    let s: String = serde_json::from_value(val)?;
    Ok(uuid::Uuid::parse_str(&s)
        .map_err(|e| AgError::bad_request_error(format!("Invalid UUID: {:?}", e)))?)
}

fn json_value_from_uuid(uuid: uuid::Uuid) -> Result<serde_json::Value, AgError> {
    Ok(serde_json::to_value(uuid.to_string())?)
}

// API handler

/**
 * Generate a handler for the API endpoint.
 * @param handle_json The function to handle the JSON request.
 * @return The handler for the API endpoint.
 */
fn gen_auth_handler(
    handle_json: impl Fn(&Json) -> Result<Json, AgError> + Send + Sync + 'static,
) -> impl Fn(Request) -> pin::Pin<Box<dyn core::future::Future<Output = Response> + Send>>
       + 'static
       + Clone
       + Send
       + 'static {
    let handle_json = sync::Arc::new(handle_json);
    move |req| {
        let handle_json = handle_json.clone();
        async fn process(
            req_meta: &axum::http::request::Parts,
            req_body: &[u8],
            res: &mut Response,
            handle_json: sync::Arc<impl Fn(&Json) -> Result<Json, AgError>>,
        ) -> Result<(), AgError> {
            if !PASSWORD.is_empty()
                && req_meta
                    .headers
                    .get("Authorization")
                    .map(|v| v.to_str().ok())
                    .flatten()
                    .unwrap_or_default()
                    != &("Bearer ".to_owned() + &*PASSWORD)
            {
                *res.status_mut() = axum::http::StatusCode::NOT_FOUND;
                return Ok(());
            }

            let msgpack = rmp_serde::to_vec(&handle_json(&if req_body.is_empty() {
                Json::Null
            } else {
                let body: Json = rmp_serde::from_slice(&req_body)?;
                body
            })?)?;
            *res.body_mut() = axum::body::Body::from(msgpack);

            Ok(())
        }
        Box::pin(async move {
            let (req_meta, req_body) = req.into_parts();
            let mut res = Response::default();
            res.headers_mut().append(
                "Content-Type",
                axum::http::HeaderValue::from_static("application/msgpack"),
            );
            let req_body = axum::body::to_bytes(req_body, usize::MAX).await.unwrap();
            if let Err(e) = process(&req_meta, &req_body, &mut res, handle_json).await {
                match e {
                    AgError::ErrorWithHttpStatus(status, msg) => {
                        *res.status_mut() = status;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg.into_owned()
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::RmpDecodeError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprintln!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                    AgError::RmpEncodeError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprintln!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                    AgError::SerdeJsonError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprintln!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                }
            }
            return res;
        })
    }
}

// entry point

#[tokio::main]
async fn main() {
    println!("");
    println!("==========================================================");
    println!("ShoutWars backend server v{} starting...", API_VER);

    let session_list = sync::Arc::new(session::session_list_t::new(
        sync::Arc::new(log_stderr),
        sync::Arc::new(log_stdout),
    ));
    let room_list = sync::Arc::new(room_list::room_list_t::new(
        (*ROOM_LIMIT) as usize,
        LOBBY_LIFETIME.to_owned(),
        GAME_LIFETIME.to_owned(),
        sync::Arc::new(log_stderr),
        sync::Arc::new(log_stdout),
    ));

    let server = axum::Router::new();

    let invalid_ver_handler = sync::Arc::new(gen_auth_handler(|_| {
        Err(AgError::not_found_error(format!(
            "Invalid API version. Use {}.",
            &*API_PATH,
        )))
    }));

    let server = server.layer(axum::middleware::from_fn(move |request: Request, next: axum::middleware::Next|
        clone_capture!([invalid_ver_handler] async move {
            if matches!(request.method(), &axum::http::Method::GET | &axum::http::Method::POST) && !request.uri().path().starts_with(&format!("{}/", &*API_PATH)) {
                invalid_ver_handler(request).await
            } else {
                next.run(request).await
            }
        })
    ));

    let server = server.route(
        &format!("{}/room/create", &*API_PATH),
        post_method(gen_auth_handler(
            clone_capture!([room_list, session_list] move |req| {
                let version: String = serde_json::from_value(req.at("version")?)?;
                let owner = room::user_t::new(&serde_json::from_value::<String>(
                    req.at("user")?.at("name")?,
                )?)?;
                let owner_id = owner.id;
                let size: usize = serde_json::from_value(req.at("size")?)?;
                let room = room_list.create(&version, owner, size)?;
                let session = session_list.create(room.id, owner_id);
                return Ok(serde_json::json!({
                    "session_id": json_value_from_uuid(session.id)?,
                    "user_id": json_value_from_uuid(owner_id)?,
                    "id": json_value_from_uuid(room.id)?,
                    "name": room.name
                }));
            }),
        )),
    );

    let server = server.route(
        &format!("{}/room/join", &*API_PATH),
        post_method(gen_auth_handler(
            clone_capture!([room_list, session_list] move |req| {
                let version: String = serde_json::from_value(req.at("version")?)?;
                let room = room_list.get(&serde_json::from_value::<String>(req.at("name")?)?)?;
                let user = room::user_t::new(&serde_json::from_value::<String>(
                    req.at("user")?.at("name")?,
                )?)?;
                let user_id = user.id;
                room.join(version, user)?;
                let session = session_list.create(room.id, user_id);
                return Ok(serde_json::json!({
                  "session_id": json_value_from_uuid(session.id)?,
                  "id": json_value_from_uuid(room.id)?,
                  "user_id": json_value_from_uuid(user_id)?,
                  "room_info": room.get_info()
                }));
            }),
        )),
    );

    let server = server.route(
        &format!("{}/room/start", &*API_PATH),
        post_method(gen_auth_handler(
            clone_capture!([room_list, session_list] move |req| {
                let session = session_list.get(&uuid_from_json_value(req.at("session_id")?)?)?;
                let room = room_list.get_by_id(&session.room_id)?;
                if session.user_id != room.get_owner()?.id {
                    return Err(AgError::forbidden_error("Only owner can start the game."));
                }
                room.start_game()?;
                return Ok(serde_json::json!({}));
            }),
        )),
    );

    let server = server.route(
        &format!("{}/room/sync", &*API_PATH),
        post_method(gen_auth_handler(clone_capture!([room_list, session_list] move |req| {
            let session = session_list.get(&uuid_from_json_value(req.at("session_id")?)?)?;
            let room = room_list.get_by_id(&session.room_id)?;
            if (time::Instant::now() - room.get_user(&session.user_id)?.get_last_time())
                < time::Duration::from_millis(100)
            {
                return Err(AgError::too_many_requests_error(
                    "Wait 100ms before sending another sync request.",
                ));
            }
            let mut user_reports = Vec::new();
            let mut user_actions = Vec::new();
            for report_j in serde_json::from_value::<Vec<Json>>(req.at("reports")?)? {
                user_reports.push(sync::Arc::new(sync_record::event_t::new(
                    uuid_from_json_value(report_j.at("id")?)?,
                    session.user_id,
                    serde_json::from_value(report_j.at("type")?)?,
                    report_j.at("event")?,
                )));
            }
            for action_j in serde_json::from_value::<Vec<Json>>(req.at("actions")?)? {
                user_actions.push(sync::Arc::new(sync_record::event_t::new(
                    uuid_from_json_value(action_j.at("id")?)?,
                    session.user_id,
                    serde_json::from_value(action_j.at("type")?)?,
                    action_j.at("event")?,
                )));
            }

            if session.user_id == room.get_owner()?.id {
                room.update_info(req.at("room_info")?);
            }
            let records = room.sync(
                session.user_id,
                &user_reports,
                &user_actions,
                time::Duration::from_millis(200),
                time::Duration::from_millis(50),
            )?;

            let mut reports = Vec::new();
            let mut actions = Vec::new();
            for record in &records {
                for report in record.get_reports() {
                    if report.from != session.user_id {
                        continue;
                    }
                    reports.push(serde_json::to_value(&*report)?);
                    if record.id != records.last().unwrap().id {
                        reports.last_mut().unwrap()["sync_id"] = json_value_from_uuid(record.id)?;
                    }
                }
                for action in record.get_actions() {
                    actions.push(serde_json::to_value(&*action)?);
                    if record.id != records.last().unwrap().id {
                        actions.last_mut().unwrap()["sync_id"] = json_value_from_uuid(record.id)?;
                    }
                }
            }
            return Ok(serde_json::json!({
              "id": json_value_from_uuid(records.last().unwrap().id)?,
              "reports": reports,
              "actions": actions,
              "room_users": room.get_users(),
            }));
        }))),
    );

    let server = server.route(
        &format!("{}/status", &*API_PATH),
        get_method(gen_auth_handler(clone_capture!([room_list] move |_| {
            return Ok(serde_json::json!({
                "room_count": room_list.count(),
                "room_limit": room_list.get_limit()
            }));
        }))),
    );

    let running = sync::Arc::new(sync::atomic::AtomicBool::new(true));
    let cleaner_thread = tokio::spawn(clone_capture!([running] async move {
        while running.load(sync::atomic::Ordering::SeqCst) {
            room_list.clean(EXPIRE_TIMEOUT);
            session_list.clean(&|session: &session::session_t| {
                return !room_list.exists_by_id(&session.room_id)
                    || !room_list
                        .get_by_id(&session.room_id)
                        .unwrap()
                        .has_user(&session.user_id);
            });
            tokio::time::sleep(CLEANER_INTERVAL).await;
        }
    }));

    println!("");
    println!("Server started at http://localhost:{}", &*PORT);
    if !PASSWORD.is_empty() {
        println!("Password: {}", &*PASSWORD);
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", *PORT))
        .await
        .unwrap();
    axum::serve(listener, server).await.unwrap();

    running.store(false, sync::atomic::Ordering::SeqCst);
    cleaner_thread.await.unwrap();

    println!("");
    println!("Server stopped");
}

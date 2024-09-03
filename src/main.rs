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
fn stoul(v: String) -> u64 {
    v.parse().unwrap()
}

mod dummy_at_method {
    pub trait Dummy {
        fn at(&self, _: &str) -> serde_json::Value {
            todo!();
        }
    }
    impl Dummy for serde_json::Value {}
    impl Dummy for &serde_json::Value {}
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
    UuidError(uuid::Error),
    NotFoundError(String),
    ForbiddenError(String),
    TooManyRequestsError(&'static str),
    RmpEncodeError(rmp_serde::encode::Error),
    RmpDecodeError(rmp_serde::decode::Error),
    BadRequestError(String),
    UnauthorizedError(&'static str),
}

impl From<serde_json::Error> for AgError {
    fn from(value: serde_json::Error) -> Self {
        Self::SerdeJsonError(value)
    }
}

impl From<uuid::Error> for AgError {
    fn from(value: uuid::Error) -> Self {
        Self::UuidError(value)
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
    Ok(uuid::Uuid::parse_str(&s)?)
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
                let body: Json = rmp_serde::from_slice(&req_body)?;
                body
            } else {
                Json::Null
            })?)?;
            *res.body_mut() = axum::body::Body::from(msgpack);
            res.headers_mut().append(
                "Content-Type",
                axum::http::HeaderValue::from_static("application/msgpack"),
            );

            Ok(())
        }
        Box::pin(async move {
            let (req_meta, req_body) = req.into_parts();
            let mut res = Response::default();
            let req_body = axum::body::to_bytes(req_body, usize::MAX).await.unwrap();
            if let Err(e) = process(&req_meta, &req_body, &mut res, handle_json).await {
                match e {
                    AgError::ForbiddenError(msg) => {
                        *res.status_mut() = axum::http::StatusCode::FORBIDDEN;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::NotFoundError(msg) => {
                        *res.status_mut() = axum::http::StatusCode::NOT_FOUND;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::TooManyRequestsError(msg) => {
                        *res.status_mut() = axum::http::StatusCode::TOO_MANY_REQUESTS;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::BadRequestError(msg) => {
                        *res.status_mut() = axum::http::StatusCode::BAD_REQUEST;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::UnauthorizedError(msg) => {
                        *res.status_mut() = axum::http::StatusCode::UNAUTHORIZED;
                        *res.body_mut() = axum::body::Body::from(
                            rmp_serde::to_vec(&serde_json::json!({
                                "error": msg
                            }))
                            .unwrap(),
                        );
                    }
                    AgError::RmpDecodeError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprint!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                    AgError::RmpEncodeError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprint!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                    AgError::SerdeJsonError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprint!(
                            "Internal server error: {:?}\n  when {} {}",
                            err, req_meta.method, req_meta.uri
                        );
                    }
                    AgError::UuidError(err) => {
                        *res.status_mut() = axum::http::StatusCode::INTERNAL_SERVER_ERROR;
                        eprint!(
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

    let session_list_arc = sync::Arc::new(session::session_list_t::new(
        sync::Arc::new(log_stderr),
        sync::Arc::new(log_stdout),
    ));
    let room_list_arc = sync::Arc::new(room_list::room_list_t::new(
        (*ROOM_LIMIT) as usize,
        LOBBY_LIFETIME.to_owned(),
        GAME_LIFETIME.to_owned(),
        sync::Arc::new(log_stderr),
        sync::Arc::new(log_stdout),
    ));

    let server = axum::Router::new();

    // server.set_exception_handler(
    //   |req: &Request, res: &Response, ep: &exception_ptr| {
    //     try {
    //       rethrow_exception(ep);
    //     } catch (const error &err) {
    //       res.status = err.code;
    //       const auto msgpack = json::to_msgpack(json{ { "error", err.what() } });
    //       res.set_content(string(msgpack.begin(), msgpack.end()), "application/msgpack");
    //     } catch (const exception &err) {
    //       res.status = 500;
    //       log_stderr(format("Internal server error: {}\n  when {} {}", err.what(), req.method, req.path));
    //     } catch (...) {
    //       res.status = 500;
    //       log_stderr(format("Unknown error\n  when {} {}", req.method, req.path));
    //     }
    //   }
    // );

    let invalid_ver_pattern = format!("((?!{}/).*)", &*API_PATH);
    let invalid_ver_handler = gen_auth_handler(|_| {
        Err(AgError::NotFoundError(format!(
            "Invalid API version. Use {}.",
            &*API_PATH,
        )))
    });
    let server = server.route(
        &invalid_ver_pattern,
        get_method(invalid_ver_handler.clone()),
    );
    let server = server.route(&invalid_ver_pattern, post_method(invalid_ver_handler));

    let room_list = room_list_arc.clone();
    let session_list = session_list_arc.clone();
    let server = server.route(
        &format!("{}/room/create", &*API_PATH),
        post_method(gen_auth_handler(move |req| {
            let version: String = serde_json::from_value(req.at("version"))?;
            let owner = room::user_t::new(&serde_json::from_value::<String>(
                req.at("user").at("name"),
            )?)?;
            let owner_id = owner.id;
            let size: usize = serde_json::from_value(req.at("size"))?;
            let room = room_list.create(&version, owner, size)?;
            let session = session_list.create(room.id, owner_id);
            return Ok(serde_json::json!({
                "session_id": json_value_from_uuid(session.id)?,
                "user_id": json_value_from_uuid(owner_id)?,
                "id": json_value_from_uuid(room.id)?,
                "name": room.name
            }));
        })),
    );

    let room_list = room_list_arc.clone();
    let session_list = session_list_arc.clone();
    let server = server.route(
        &format!("{}/room/join", &*API_PATH),
        post_method(gen_auth_handler(move |req| {
            let version: String = serde_json::from_value(req.at("version"))?;
            let room = room_list.get(&serde_json::from_value::<String>(req.at("name"))?)?;
            let user = room::user_t::new(&serde_json::from_value::<String>(
                req.at("user").at("name"),
            )?)?;
            let user_id = user.id.clone();
            room.join(version, user)?;
            let session = session_list.create(room.id, user_id);
            return Ok(serde_json::json!({
              "session_id": json_value_from_uuid(session.id)?,
              "id": json_value_from_uuid(room.id)?,
              "user_id": json_value_from_uuid(user_id)?,
              "room_info": room.get_info()
            }));
        })),
    );

    let room_list = room_list_arc.clone();
    let session_list = session_list_arc.clone();
    let server = server.route(
        &format!("{}/room/start", &*API_PATH),
        post_method(gen_auth_handler(move |req| {
            let session = session_list.get(&uuid_from_json_value(req.at("session_id"))?)?;
            let room = room_list.get_by_id(&session.room_id)?;
            if session.user_id != room.get_owner()?.id {
                return Err(AgError::ForbiddenError(
                    "Only owner can start the game.".to_owned(),
                ));
            }
            room.start_game()?;
            return Ok(serde_json::json!({}));
        })),
    );

    let room_list = room_list_arc.clone();
    let session_list = session_list_arc.clone();
    let server = server.route(
        &format!("{}/room/sync", &*API_PATH),
        post_method(gen_auth_handler(move |req| {
            let session = session_list.get(&uuid_from_json_value(req.at("session_id"))?)?;
            let room = room_list.get_by_id(&session.room_id)?;
            if (time::Instant::now() - room.get_user(&session.user_id)?.get_last_time())
                < time::Duration::from_millis(100)
            {
                return Err(AgError::TooManyRequestsError(
                    "Wait 100ms before sending another sync request.",
                ));
            }
            let mut user_reports = Vec::new();
            let mut user_actions = Vec::new();
            for report_j in serde_json::from_value::<Vec<Json>>(req.at("reports"))? {
                user_reports.push(sync::Arc::new(sync_record::event_t::new(
                    uuid_from_json_value(report_j.at("id"))?,
                    session.user_id,
                    serde_json::from_value(report_j.at("type"))?,
                    report_j.at("event"),
                )));
            }
            for action_j in serde_json::from_value::<Vec<Json>>(req.at("actions"))? {
                user_actions.push(sync::Arc::new(sync_record::event_t::new(
                    uuid_from_json_value(action_j.at("id"))?,
                    session.user_id,
                    serde_json::from_value(action_j.at("type"))?,
                    action_j.at("event"),
                )));
            }

            if session.user_id == room.get_owner()?.id {
                room.update_info(req.at("room_info"));
            }
            let records = room.sync(
                &session.user_id,
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
        })),
    );

    let room_list = room_list_arc.clone();
    let server = server.route(
        &format!("{}/status", &*API_PATH),
        get_method(gen_auth_handler(move |_| {
            return Ok(serde_json::json!({
                "room_count": room_list.count(),
                "room_limit": room_list.get_limit()
            }));
        })),
    );

    let running_arc = sync::Arc::new(sync::atomic::AtomicBool::new(true));
    let running = running_arc.clone();
    let cleaner_thread = tokio::spawn(async move {
        while running.load(sync::atomic::Ordering::SeqCst) {
            room_list_arc.clean(EXPIRE_TIMEOUT);
            session_list_arc.clean(&|session: &session::session_t| {
                return !room_list_arc.exists_by_id(&session.room_id)
                    || !room_list_arc
                        .get_by_id(&session.room_id)
                        .unwrap()
                        .has_user(&session.user_id);
            });
            tokio::time::sleep(CLEANER_INTERVAL).await;
        }
    });

    println!("");
    println!("Server started at http://localhost:{}", &*PORT);
    if !PASSWORD.is_empty() {
        println!("Password: {}", &*PASSWORD);
    }

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", *PORT))
        .await
        .unwrap();
    axum::serve(listener, server).await.unwrap();

    running_arc.store(false, sync::atomic::Ordering::SeqCst);
    cleaner_thread.await.unwrap();

    println!("");
    println!("Server stopped");
}

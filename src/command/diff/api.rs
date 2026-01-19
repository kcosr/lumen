use std::io;
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub enum ApiCommand {
    Status {
        respond_to: Sender<ApiResponse>,
    },
    CurrentHunk {
        respond_to: Sender<ApiResponse>,
    },
    CurrentFile {
        respond_to: Sender<ApiResponse>,
    },
    FullContext {
        respond_to: Sender<ApiResponse>,
    },
    Annotations {
        current_only: bool,
        respond_to: Sender<ApiResponse>,
    },
    CreateAnnotation {
        content: String,
        respond_to: Sender<ApiResponse>,
    },
}

#[derive(Debug)]
pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
}

pub fn start_api_server(
    bind: &str,
    command_tx: Sender<ApiCommand>,
) -> io::Result<thread::JoinHandle<()>> {
    let server = Server::http(bind)
        .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
    let bind_display = bind.to_string();
    let handle = thread::spawn(move || {
        for mut request in server.incoming_requests() {
            let method = request.method().clone();
            let path = request.url().split('?').next().unwrap_or("");

            let response = match (method, path) {
                (Method::Get, "/status") => dispatch(&command_tx, |respond_to| ApiCommand::Status {
                    respond_to,
                }),
                (Method::Get, "/current-hunk") => {
                    dispatch(&command_tx, |respond_to| ApiCommand::CurrentHunk { respond_to })
                }
                (Method::Get, "/current-file") => {
                    dispatch(&command_tx, |respond_to| ApiCommand::CurrentFile { respond_to })
                }
                (Method::Get, "/full-context") => {
                    dispatch(&command_tx, |respond_to| ApiCommand::FullContext { respond_to })
                }
                (Method::Get, "/annotations") => dispatch(&command_tx, |respond_to| {
                    ApiCommand::Annotations {
                        current_only: false,
                        respond_to,
                    }
                }),
                (Method::Get, "/annotations/current") => dispatch(&command_tx, |respond_to| {
                    ApiCommand::Annotations {
                        current_only: true,
                        respond_to,
                    }
                }),
                (Method::Post, "/annotation/create") => {
                    let mut body = String::new();
                    let reader = request.as_reader();
                    if reader.read_to_string(&mut body).is_err() {
                        ApiResponse {
                            status: 400,
                            body: json_error("invalid request body"),
                        }
                    } else {
                        match serde_json::from_str::<Value>(&body)
                            .ok()
                            .and_then(|v| v.get("content").and_then(|c| c.as_str()).map(String::from))
                        {
                            Some(content) => dispatch(&command_tx, |respond_to| {
                                ApiCommand::CreateAnnotation { content, respond_to }
                            }),
                            None => ApiResponse {
                                status: 400,
                                body: json_error("missing content field"),
                            },
                        }
                    }
                }
                _ => ApiResponse {
                    status: 404,
                    body: json_error("not found"),
                },
            };

            let status = StatusCode(response.status);
            let body = response.body.to_string();
            let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
                .unwrap();
            let mut http_response = Response::from_string(body).with_status_code(status);
            http_response.add_header(header);
            let _ = request.respond(http_response);
        }
    });

    eprintln!("Lumen API listening on http://{}", bind_display);
    Ok(handle)
}

fn dispatch<F>(command_tx: &Sender<ApiCommand>, build: F) -> ApiResponse
where
    F: FnOnce(Sender<ApiResponse>) -> ApiCommand,
{
    let (response_tx, response_rx) = mpsc::channel::<ApiResponse>();
    let command = build(response_tx);

    if command_tx.send(command).is_err() {
        return ApiResponse {
            status: 503,
            body: json_error("api handler unavailable"),
        };
    }

    match response_rx.recv_timeout(RESPONSE_TIMEOUT) {
        Ok(response) => response,
        Err(_) => ApiResponse {
            status: 504,
            body: json_error("request timed out"),
        },
    }
}

fn json_error(message: &str) -> Value {
    serde_json::json!({ "error": message })
}

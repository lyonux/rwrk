use axum::{
    Json,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    http::StatusCode,
    response::Response,
};

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};

pub mod handler {

    use axum::extract::Query;

    use super::*;

    // the input to our `post_data` handler
    #[derive(Deserialize, Serialize)]
    pub struct PostData {
        pub data: String,
    }

    #[derive(Deserialize, Serialize)]
    pub struct Params {
        pub sleep: Option<String>,
    }

    // basic handler that responds with a static string
    pub async fn check(Query(params): Query<Params>) -> &'static str {
        match params.sleep {
            Some(sleep) => {
                let _ =
                    tokio::time::sleep(std::time::Duration::from_secs(sleep.parse().unwrap_or(0)))
                        .await;
                "ok!"
            }
            None => "ok!",
        }
    }
    pub async fn ping() -> &'static str {
        "pong"
    }

    pub async fn post_data(
        // this argument tells axum to parse the request body
        // as JSON into a `PostData` type
        Json(payload): Json<PostData>,
    ) -> (StatusCode, Json<PostData>) {
        // insert your application logic here
        let data = PostData { data: payload.data };

        println!("data ====> {}", data.data);

        // this will be converted into a JSON response
        // with a status code of `201 Created`
        (StatusCode::CREATED, Json(data))
    }

    pub async fn ws_echo(ws: WebSocketUpgrade) -> Response {
        ws.on_upgrade(handle_socket)
    }

    async fn handle_socket(mut socket: WebSocket) {
        while let Some(result) = socket.next().await {
            match result {
                Ok(msg) => match msg {
                    Message::Text(text) => {
                        if socket
                            .send(Message::Text(format!("Echo: {}", text).into()))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Message::Close(_) => {
                        break;
                    }
                    _ => {}
                },
                Err(_) => break,
            }
        }
    }
}

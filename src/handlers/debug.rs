use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;

use crate::handlers::admin::require_admin;
use crate::state::AppState;

pub(crate) async fn get_debug_data(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    if let Some(data) = state.debug_data.get().await {
        HttpResponse::Ok().json(data)
    } else {
        HttpResponse::Ok().json(json!({
            "client_request": null,
            "upstream_request": null,
            "upstream_response": null
        }))
    }
}

pub(crate) async fn clear_debug_data(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    state.debug_data.data.write().await.take();
    HttpResponse::Ok().json(json!({"success": true}))
}

/// SSE 流式调试端点
pub(crate) async fn debug_stream(state: web::Data<AppState>, req: HttpRequest) -> HttpResponse {
    if let Err(resp) = require_admin(&state, &req).await {
        return resp;
    }
    let stream = state.stream_hub.create_stream();

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .body(actix_web::body::BodyStream::new(stream))
}

pub(crate) async fn webui_index() -> HttpResponse {
    HttpResponse::Found()
        .append_header(("Location", "/webui/index.html"))
        .finish()
}

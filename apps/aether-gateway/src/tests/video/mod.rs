use std::sync::{Arc, Mutex};
use std::thread;

use aether_data::repository::video_tasks::InMemoryVideoTaskRepository;
use aether_data_contracts::repository::video_tasks::{
    UpsertVideoTask, VideoTaskLookupKey, VideoTaskReadRepository, VideoTaskWriteRepository,
};
use axum::body::{to_bytes, Body, Bytes};
use axum::response::Response;
use axum::routing::any;
use axum::{extract::Request, Json, Router};
use http::header::{HeaderName, HeaderValue};
use http::StatusCode;
use serde_json::json;

use crate::constants::{
    CONTROL_EXECUTED_HEADER, CONTROL_EXECUTE_FALLBACK_HEADER, EXECUTION_PATH_HEADER,
    TRACE_ID_HEADER,
};

use super::{
    build_router, build_router_with_state, build_state_with_execution_runtime_override,
    start_server, AppState, VideoTaskTruthSourceMode,
};

mod data_read;
mod gemini_sync_create;
mod gemini_sync_task;
mod openai_sync_create;
mod openai_sync_task;
mod registry_poller;
mod routing;
mod stream;

const VIDEO_ROUTE_TEST_STACK_BYTES: usize = 32 * 1024 * 1024;

fn run_video_route_test<F, Fut>(name: &'static str, test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_string())
        .stack_size(VIDEO_ROUTE_TEST_STACK_BYTES)
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("video route test runtime should build")
                .block_on(test());
        })
        .expect("video route test thread should spawn")
        .join()
        .expect("video route test thread should finish");
}

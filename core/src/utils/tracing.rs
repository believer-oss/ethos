use std::time::Duration;
use axum::extract::MatchedPath;
use axum::http::{HeaderMap, Request, Response};
use tower_http::trace::{HttpMakeClassifier, MakeSpan, OnBodyChunk, OnFailure};
use tower_http::trace::{OnEos, OnRequest, OnResponse, TraceLayer};
use tracing::field::Empty;
use tracing::Span;

/// Creates a new [`TraceLayer`] that traces HTTP requests.
pub fn new_tracing_layer(app: String) -> TraceLayer<HttpMakeClassifier, TraceMakeSpan, TraceOnRequest, TraceOnResponse, TraceOnBodyChunk, TraceOnEos, TraceOnFailure> {
    opentelemetry::global::set_text_map_propagator(
        opentelemetry_sdk::propagation::TraceContextPropagator::new(),
    );

    TraceLayer::new_for_http()
        .make_span_with(TraceMakeSpan::new(app))
        .on_request(TraceOnRequest)
        .on_response(TraceOnResponse)
        .on_body_chunk(TraceOnBodyChunk)
        .on_eos(TraceOnEos)
        .on_failure(TraceOnFailure)
}
#[derive(Clone)]
pub struct TraceMakeSpan {
    app: String,
}

impl TraceMakeSpan {
    fn new(app: String) -> Self {
        Self { app }
    }
}

impl<B> MakeSpan<B> for TraceMakeSpan {
    fn make_span(&mut self, request: &Request<B>) -> Span {
        let matched_path = request
            .extensions()
            .get::<MatchedPath>()
            .map(MatchedPath::as_str);

        let span = tracing::info_span!(
            "HTTP request",
            http.method = ?request.method(),
            http.path = matched_path,
            http.response.body.size = Empty,
            http.status_code = Empty,
            otel.kind = "client",
            otel.status_code = "ok",
            otel.status_message = Empty,
            service.name = &self.app,
            trace_id = Empty,
        );

        span
    }
}

#[derive(Clone, Debug)]
pub struct TraceOnRequest;

impl<B> OnRequest<B> for TraceOnRequest {
    fn on_request(&mut self, _request: &Request<B>, _span: &Span) {
        tracing::event!(
            tracing::Level::DEBUG,
            "request started"
        );
    }
}

#[derive(Clone, Debug)]
pub struct TraceOnResponse;

impl<B> OnResponse<B> for TraceOnResponse {
    fn on_response(self, response: &Response<B>, latency: Duration, span: &Span) {
        tracing::event!(
            tracing::Level::DEBUG,
            duration = latency.as_micros(),
            "response produced"
        );
        span.record("http.status_code", response.status().as_u16());
        span.record(
            "http.response.body.size",
            content_length_as_usize(response.headers()),
        );

        if response.status().is_server_error() {
            span.record("otel.status_code", "error");
        }
    }
}

#[derive(Clone, Debug)]
pub struct TraceOnBodyChunk;

impl<B> OnBodyChunk<B> for TraceOnBodyChunk {
    fn on_body_chunk(&mut self, _chunk: &B, latency: Duration, _span: &Span) {
        tracing::event!(
            tracing::Level::DEBUG,
            duration = latency.as_micros(),
            "response body chunk sent"
        );
    }
}

#[derive(Clone, Debug)]
pub struct TraceOnEos;

impl OnEos for TraceOnEos {
    fn on_eos(self, trailers: Option<&HeaderMap>, stream_duration: Duration, _span: &Span) {
        tracing::event!(
            tracing::Level::DEBUG,
            duration = stream_duration.as_micros(),
            trailers = ?trailers,
            "response stream ended"
        );
    }
}

#[derive(Clone, Debug)]
pub struct TraceOnFailure;

impl<T> OnFailure<T> for TraceOnFailure
where
    T: std::fmt::Display,
{
    fn on_failure(&mut self, _failure: T, _latency: Duration, _span: &Span) {
        tracing::event!(
            tracing::Level::DEBUG,
            "request failure"
        );
    }
}

pub(crate) fn content_length_as_usize(headers: &HeaderMap) -> usize {
    headers
        .get("content-length")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or_default()
}

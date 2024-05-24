use axum::body::Body;
use axum::http::{Request, Uri};
use axum::middleware::Next;
use axum::response::Response;

#[derive(Clone)]
pub struct RequestUri(pub Uri);

pub async fn uri_passthrough(request: Request<Body>, next: Next) -> Response {
    let uri = request.uri().clone();

    let mut response = next.run(request).await;
    response.extensions_mut().insert(RequestUri(uri));

    response
}

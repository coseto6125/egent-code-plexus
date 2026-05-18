use axum::{routing::{get, post}, Router};

async fn create_user() {}
async fn get_user() {}

fn router() -> Router {
    Router::new()
        .route("/api/users", post(create_user))
        .route("/api/users/:id", get(get_user))
}

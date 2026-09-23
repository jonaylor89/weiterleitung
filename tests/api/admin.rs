//! The Axum admin area.

use crate::harness::{TestApp, spawn_app};

#[tokio::test]
async fn health_check_works() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .get(format!("{}/health_check", app.address))
        .send()
        .await
        .expect("Failed to execute the request");

    assert!(response.status().is_success());
}

#[tokio::test]
async fn the_admin_area_redirects_anonymous_visitors_to_the_login_page() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .get(format!("{}/admin/dashboard", app.address))
        .send()
        .await
        .expect("Failed to execute the request");

    assert_eq!(response.status().as_u16(), 303);
    assert_eq!(response.headers()["Location"], "/login");
}

#[tokio::test]
async fn the_configured_admin_can_log_in_and_reach_the_dashboard() {
    let app = spawn_app().await;
    let client = TestApp::client();

    let response = client
        .post(format!("{}/login", app.address))
        .form(&serde_json::json!({"username": "admin", "password": "password"}))
        .send()
        .await
        .expect("Failed to execute the request");
    assert_eq!(response.headers()["Location"], "/admin/dashboard");

    let body = client
        .get(format!("{}/admin/dashboard", app.address))
        .send()
        .await
        .expect("Failed to execute the request")
        .text()
        .await
        .expect("Failed to read the response body");
    assert!(body.contains("new alias"));
}

#[tokio::test]
async fn wrong_credentials_are_rejected() {
    let app = spawn_app().await;

    let response = TestApp::client()
        .post(format!("{}/login", app.address))
        .form(&serde_json::json!({"username": "admin", "password": "wrong"}))
        .send()
        .await
        .expect("Failed to execute the request");

    assert_eq!(response.headers()["Location"], "/login");
}

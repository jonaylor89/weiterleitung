use axum::routing::{get, post};
use axum::{Router, middleware, serve::Serve};
use secrecy::ExposeSecret;
use sqlx::SqlitePool;
use sqlx::sqlite::SqlitePoolOptions;
use time::Duration;
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tower_sessions::cookie::Key;
use tower_sessions::service::PrivateCookie;
use tower_sessions::{Expiry, SessionManagerLayer};
use tower_sessions_sqlx_store::SqliteStore;

use crate::authentication::{AuthenticatedUser, seed_admin_user};
use crate::configuration::{AliasSettings, DatabaseSettings, Settings};
use crate::routes::{
    alias_detail, app_css, app_js, create_alias, create_mailbox, dashboard, delete_alias,
    delete_mailbox, health_check, home, log_out, login, login_form, mailboxes, styleguide,
    toggle_alias,
};

#[derive(Clone)]
pub struct AppState {
    pub db_pool: SqlitePool,
    pub aliases: AliasSettings,
}

impl axum::extract::FromRef<AppState> for SqlitePool {
    fn from_ref(state: &AppState) -> Self {
        state.db_pool.clone()
    }
}

pub struct Application {
    port: u16,
    server: Serve<TcpListener, Router, Router>,
}

impl Application {
    pub async fn build(configuration: Settings) -> Result<Self, anyhow::Error> {
        let db_pool = get_connection_pool(&configuration.database).await;
        sqlx::migrate!("./migrations").run(&db_pool).await?;
        seed_admin_user(
            &db_pool,
            &configuration.admin.username,
            configuration.admin.password.clone(),
        )
        .await?;

        let session_store = SqliteStore::new(db_pool.clone())
            .with_table_name("sessions")
            .map_err(anyhow::Error::msg)?;
        session_store.migrate().await?;
        let key = Key::derive_from(
            configuration
                .application
                .hmac_secret
                .expose_secret()
                .as_bytes(),
        );
        let session_layer = SessionManagerLayer::new(session_store)
            .with_private(key)
            .with_expiry(Expiry::OnInactivity(Duration::days(30)));

        let address = format!(
            "{}:{}",
            configuration.application.host, configuration.application.port
        );
        let listener = TcpListener::bind(&address).await?;
        let port = listener.local_addr()?.port();
        tracing::info!("API server listening on {address}");

        let state = AppState {
            db_pool,
            aliases: configuration.aliases.clone(),
        };
        let server = run(listener, state, session_layer)?;

        Ok(Self { port, server })
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub async fn run_until_stopped(self) -> Result<(), std::io::Error> {
        self.server.await
    }
}

fn build_router(
    session_layer: SessionManagerLayer<SqliteStore, PrivateCookie>,
) -> Router<AppState> {
    let admin_routes = Router::<AppState>::new()
        .route("/dashboard", get(dashboard))
        .route("/aliases", post(create_alias))
        .route("/aliases/{alias_id}", get(alias_detail))
        .route("/aliases/{alias_id}/toggle", post(toggle_alias))
        .route("/aliases/{alias_id}/delete", post(delete_alias))
        .route("/mailboxes", get(mailboxes).post(create_mailbox))
        .route("/mailboxes/{mailbox_id}/delete", post(delete_mailbox))
        .route("/styleguide", get(styleguide))
        .route("/logout", post(log_out))
        .route_layer(middleware::from_extractor::<AuthenticatedUser>());

    Router::<AppState>::new()
        .route("/", get(home))
        .route("/health_check", get(health_check))
        .route("/login", get(login_form).post(login))
        .route("/static/app.css", get(app_css))
        .route("/static/app.js", get(app_js))
        .nest("/admin", admin_routes)
        .layer(session_layer)
        .layer(TraceLayer::new_for_http())
}

fn run(
    listener: TcpListener,
    state: AppState,
    session_layer: SessionManagerLayer<SqliteStore, PrivateCookie>,
) -> Result<Serve<TcpListener, Router, Router>, anyhow::Error> {
    let app: Router = build_router(session_layer).with_state::<()>(state);
    Ok(axum::serve(listener, app))
}

pub async fn get_connection_pool(configuration: &DatabaseSettings) -> SqlitePool {
    if let Some(parent) = std::path::Path::new(&configuration.path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    SqlitePoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(2))
        .connect(&configuration.connection_string())
        .await
        .expect("Failed to create the SQLite connection pool")
}

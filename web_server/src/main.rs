use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, post, get, Router};
use axum_prometheus::PrometheusMetricLayer;
use database_schema::{notifications, Notification};
use diesel::prelude::*;
use diesel_async::pooled_connection::deadpool::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use tower_governor::key_extractor;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::task::JoinSet;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tracing::{debug, error, info, span, warn, Instrument, Level};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::Layer;
use url::Url;

async fn get_notifications(
    State(pool): State<Pool<AsyncPgConnection>>,
    Path(user_id): Path<String>,
) -> Result<String, StatusCode> {
    info!("Getting notifications for user: {}", user_id);
    let mut conn = pool
        .get()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    let results = notifications::table
        .filter(notifications::user_id.eq(&user_id))
        .select(Notification::as_select())
        .load(&mut conn)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    info!("Found {} notifications for user: {}", results.len(), user_id);

    Ok(serde_json::to_string(&results).unwrap())
}

async fn clear_notifications(
    State(pool): State<Pool<AsyncPgConnection>>,
    Path(user_id): Path<String>,
) -> Result<String, StatusCode> {
    info!("Clearing notifications for user: {}", user_id);
    let mut conn = pool
        .get()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    diesel::delete(notifications::table.filter(notifications::user_id.eq(&user_id)))
        .execute(&mut conn)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok("Success".to_string())
}

async fn delete_notification(
    State(pool): State<Pool<AsyncPgConnection>>,
    Path((user_id, notification_id)): Path<(String, i32)>,
) -> Result<String, StatusCode> {
    info!(
        "deleting notification {} for user: {}",
        notification_id, user_id
    );
    let mut conn = pool
        .get()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;

    diesel::delete(notifications::dsl::notifications.find(notification_id))
        .execute(&mut conn)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok("Success".to_string())
}

async fn _main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let loki_url = std::env::var("LOKI_URL");
    if let Ok(loki_url) = loki_url {
        let environment = std::env::var("ENVIRONMENT").unwrap_or("dev".to_string());
        let (layer, task) = tracing_loki::builder()
            .label("environment", environment)?
            .label("service_name", "notifier")?
            .extra_field("pid", format!("{}", std::process::id()))?
            .build_url(Url::parse(&loki_url).unwrap())?;

        tracing_subscriber::registry()
            .with(layer.with_filter(tracing_subscriber::filter::EnvFilter::from_default_env()))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(std::io::stdout)
                    .with_filter(tracing_subscriber::filter::EnvFilter::from_default_env()),
            )
            .init();

        tokio::spawn(task);
        tracing::info!("Web Server starting, loki tracing enabled.");
    } else {
        error!("LOKI_URL not set, will not send logs to Loki");
        tracing_subscriber::fmt::init();
    }

    info!("Getting DB");
    let pg_url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set");
    let pg_config = AsyncDieselConnectionManager::<diesel_async::AsyncPgConnection>::new(&pg_url);
    let pg_pool = Pool::builder(pg_config).build()?;
    info!("Got DB");

    let governor_conf = Arc::new(
        GovernorConfigBuilder::default()
            .key_extractor(key_extractor::SmartIpKeyExtractor {})
            .per_second(10)
            .burst_size(5)
            .finish()
            .unwrap(),
    );
    let governor_limiter = governor_conf.limiter().clone();
    let cleanup_interval = Duration::from_secs(60);
    // a separate background task to clean up
    std::thread::spawn(move || loop {
        std::thread::sleep(cleanup_interval);
        governor_limiter.retain_recent();
    });
    let axum_url = std::env::var("BIND_WEB").unwrap_or("0.0.0.0:8004".to_string());

    let (prometheus_layer, metric_handle) = PrometheusMetricLayer::pair();

    let axum_app = Router::new()
        .route("/", get(|| async { "Web server online." }))
        .route("/metrics", get(|| async move { metric_handle.render() }))
        .route("/notifications/{user_id}", get(get_notifications))
        .route(
            "/notifications/{user_id}/clear",
            delete(clear_notifications),
        )
        .route(
            "/notifications/{user_id}/{notification_id}",
            delete(delete_notification),
        )
        .with_state(pg_pool)
        .layer(GovernorLayer {
            config: governor_conf,
        })
        .layer(prometheus_layer);

    let addr = axum_url.parse::<SocketAddr>().unwrap();
    tracing::debug!("listening on {}", addr);
    let listener = TcpListener::bind(addr).await.unwrap();

    let mut tasks: JoinSet<_> = JoinSet::new();
    tasks.spawn(async move {
        axum::serve(
            listener,
            axum_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });

    tasks.join_all().await;
    Ok(())
}

fn main() {
    let sentry_dsn = std::env::var("SENTRY_DSN");
    if sentry_dsn.is_ok() && !sentry_dsn.as_ref().unwrap().is_empty() {
        let _guard = sentry::init((
            sentry_dsn.ok(),
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ));
    }

    let result = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(_main());

    if let Err(e) = result {
        eprintln!("Error: {:?}", e);
    }
}

//! Ошибки `andler-store`.
//!
//! Сознательно не пытаемся различать "транзиентную" ошибку sqlite (занятая
//! база, lock) от "постоянной" (повреждённый файл) на этом уровне — вызывающая
//! сторона (`andler-daemon`) пока не делает retry-логику, а `rusqlite::Error`
//! и так несёт достаточно деталей через `#[source]` для логов/диагностики.

use andler_core::InstanceId;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StoreError {
    /// Запрошенный `InstanceId` не найден в таблице `instances`. Не путать с
    /// `andler-daemon::DaemonError::InstanceNotFound` — это разные слои
    /// (персистентность vs in-memory реестр); `andler-daemon` транслирует
    /// эту ошибку в свою при восстановлении/обновлении.
    #[error("instance {0:?} not found in store")]
    NotFound(InstanceId),

    /// Любая ошибка sqlite (подключение, запрос, миграция). `rusqlite::Error`
    /// уже Display-формат включает достаточно контекста, поэтому не
    /// разбираем его на подварианты.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// `InstanceConfig`/`InstanceState` не удалось (де)сериализовать в/из
    /// JSON. На практике это означает рассинхрон версий схемы между тем, что
    /// записано в базе, и текущим `andler-core` — не ошибка пользователя.
    #[error("failed to (de)serialize stored instance data: {0}")]
    Serde(#[from] serde_json::Error),

    /// Фоновая блокирующая задача (`tokio::task::spawn_blocking`), в которой
    /// выполнялся запрос к sqlite, не была дождана — паника в задаче или
    /// отмена. Это не ошибка sqlite/данных, а сбой самого механизма
    /// выполнения запроса.
    #[error("blocking store task failed to complete: {0}")]
    TaskJoin(#[from] tokio::task::JoinError),
}

//! Конвертации между сгенерированными `proto`-типами и доменными типами
//! `andler-core`. Каждое направление явное (`From`/`TryFrom`), без `serde`
//! на proto-типах — это намеренно ручной, видимый код, а не магия
//! derive-макроса поверх protobuf-сообщений.

use crate::proto;
use andler_core::{AndroidProfile, AndroidVersion, InstanceState, RootMode};

/// Ошибка конвертации proto-сообщения в доменный тип — на практике сейчас
/// только "пришло значение enum'а, для которого нет соответствия"
/// (`ANDROID_VERSION_UNSPECIFIED`/`ROOT_MODE_UNSPECIFIED`, либо вообще не
/// входящее в диапазон известных `prost` значение).
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    #[error("missing or unspecified android_version")]
    MissingAndroidVersion,
    #[error("missing or unspecified root_mode")]
    MissingRootMode,
    #[error("missing instance_id")]
    MissingInstanceId,
    #[error("invalid instance_id {0:?}: {1}")]
    InvalidInstanceId(String, uuid::Error),
}

impl TryFrom<proto::AndroidVersion> for AndroidVersion {
    type Error = ConvertError;

    fn try_from(value: proto::AndroidVersion) -> Result<Self, Self::Error> {
        match value {
            proto::AndroidVersion::Android11 => Ok(AndroidVersion::Android11),
            proto::AndroidVersion::Android13 => Ok(AndroidVersion::Android13),
            proto::AndroidVersion::Unspecified => Err(ConvertError::MissingAndroidVersion),
        }
    }
}

impl From<AndroidVersion> for proto::AndroidVersion {
    fn from(value: AndroidVersion) -> Self {
        match value {
            AndroidVersion::Android11 => proto::AndroidVersion::Android11,
            AndroidVersion::Android13 => proto::AndroidVersion::Android13,
        }
    }
}

impl TryFrom<proto::RootMode> for RootMode {
    type Error = ConvertError;

    fn try_from(value: proto::RootMode) -> Result<Self, Self::Error> {
        match value {
            proto::RootMode::None => Ok(RootMode::None),
            proto::RootMode::Magisk => Ok(RootMode::Magisk),
            proto::RootMode::KernelSu => Ok(RootMode::KernelSu),
            proto::RootMode::Unspecified => Err(ConvertError::MissingRootMode),
        }
    }
}

impl From<RootMode> for proto::RootMode {
    fn from(value: RootMode) -> Self {
        match value {
            RootMode::None => proto::RootMode::None,
            RootMode::Magisk => proto::RootMode::Magisk,
            RootMode::KernelSu => proto::RootMode::KernelSu,
        }
    }
}

impl TryFrom<proto::AndroidProfile> for AndroidProfile {
    type Error = ConvertError;

    fn try_from(value: proto::AndroidProfile) -> Result<Self, Self::Error> {
        Ok(AndroidProfile {
            android_version: value.android_version().try_into()?,
            gapps: value.gapps,
            microg: value.microg,
            libndk: value.libndk,
            root: value.root().try_into()?,
        })
    }
}

impl From<AndroidProfile> for proto::AndroidProfile {
    fn from(value: AndroidProfile) -> Self {
        let mut msg = proto::AndroidProfile {
            gapps: value.gapps,
            microg: value.microg,
            libndk: value.libndk,
            ..Default::default()
        };
        msg.set_android_version(value.android_version.into());
        msg.set_root(value.root.into());
        msg
    }
}

/// Парсит `instance_id` из proto-запроса (строка с UUID) в `InstanceId`.
/// Отдельная функция, а не `impl TryFrom<String> for InstanceId` в
/// `andler-core` — этот формат (строка из gRPC-запроса) специфичен для
/// протокола, не часть домена.
pub fn parse_instance_id(raw: &str) -> Result<andler_core::InstanceId, ConvertError> {
    if raw.is_empty() {
        return Err(ConvertError::MissingInstanceId);
    }
    uuid::Uuid::parse_str(raw)
        .map(andler_core::InstanceId)
        .map_err(|source| ConvertError::InvalidInstanceId(raw.to_string(), source))
}

/// Заполняет `InstanceStatusResponse` из `InstanceState` — `detail`
/// (диагностика backend'а, не часть FSM) заполняется отдельно вызывающей
/// стороной (`andler-daemon::service`), так как `InstanceState` сам по себе
/// её не несёт (см. `andler_core::backend::BackendStatus`).
pub fn instance_state_to_proto(state: &InstanceState) -> (proto::InstanceStateKind, String) {
    match state {
        InstanceState::Created => (proto::InstanceStateKind::Created, String::new()),
        InstanceState::Starting => (proto::InstanceStateKind::Starting, String::new()),
        InstanceState::Running => (proto::InstanceStateKind::Running, String::new()),
        InstanceState::Paused => (proto::InstanceStateKind::Paused, String::new()),
        InstanceState::Stopping => (proto::InstanceStateKind::Stopping, String::new()),
        InstanceState::Stopped => (proto::InstanceStateKind::Stopped, String::new()),
        InstanceState::Error { message } => {
            (proto::InstanceStateKind::Error, message.clone())
        }
    }
}

/// `Status` (`tonic`) — чужой тип для `andler-rpc`, но `ConvertError` —
/// локальный, так что `impl ForeignTrait<LocalType> for ForeignType` здесь
/// разрешён orphan rule (в отличие от попытки сделать то же самое в
/// `andler-daemon`, где оба типа чужие). Это и есть причина, по которой
/// конвертации лежат в `andler-rpc`, а не в крейте, который их использует.
impl From<ConvertError> for tonic::Status {
    fn from(err: ConvertError) -> Self {
        tonic::Status::invalid_argument(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn android_profile_round_trips_through_proto() {
        let profile = AndroidProfile {
            android_version: AndroidVersion::Android13,
            gapps: true,
            microg: false,
            libndk: true,
            root: RootMode::Magisk,
        };

        let msg: proto::AndroidProfile = profile.clone().into();
        let back: AndroidProfile = msg.try_into().unwrap();
        assert_eq!(profile, back);
    }

    #[test]
    fn unspecified_android_version_is_rejected() {
        let msg = proto::AndroidProfile {
            android_version: proto::AndroidVersion::Unspecified as i32,
            gapps: false,
            microg: false,
            libndk: false,
            root: proto::RootMode::None as i32,
        };
        let err = AndroidProfile::try_from(msg).unwrap_err();
        assert!(matches!(err, ConvertError::MissingAndroidVersion));
    }

    #[test]
    fn parse_instance_id_rejects_empty_string() {
        let err = parse_instance_id("").unwrap_err();
        assert!(matches!(err, ConvertError::MissingInstanceId));
    }

    #[test]
    fn parse_instance_id_rejects_garbage() {
        let err = parse_instance_id("not-a-uuid").unwrap_err();
        assert!(matches!(err, ConvertError::InvalidInstanceId(_, _)));
    }

    #[test]
    fn parse_instance_id_accepts_valid_uuid() {
        let id = andler_core::InstanceId::new();
        let parsed = parse_instance_id(&id.0.to_string()).unwrap();
        assert_eq!(parsed, id);
    }

    #[test]
    fn error_state_carries_message_into_detail_tuple() {
        let state = InstanceState::Error {
            message: "boom".to_string(),
        };
        let (kind, message) = instance_state_to_proto(&state);
        assert_eq!(kind, proto::InstanceStateKind::Error);
        assert_eq!(message, "boom");
    }
}

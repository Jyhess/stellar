//! Translation of the errors of the lower layers. Each variant has its text
//! here (exhaustive `match`: a new variant does not compile without one);
//! text from outside (system, driver, serializer) is quoted untranslated.

use stellar_application::{AppError, ContinuationError, FormatError, RepositoryError};
use stellar_domain::EngineError;

use crate::format::count;
use crate::i18n::tr;

/// Text of an error in the interface's language.
pub trait Localized {
    fn localized(&self) -> String;
}

impl Localized for EngineError {
    fn localized(&self) -> String {
        match self {
            Self::NoDevice => tr("no compatible graphics card was found").to_owned(),
            Self::ComputeUnsupported => tr("the graphics card does not support compute shaders").to_owned(),
            Self::DeviceOpen(e) => tr!("cannot open the graphics card: {}", e),
            Self::TooManyBodies { requested, limit } => tr!(
                "the computation engine is limited to {} bodies ({} requested)",
                count(*limit as u64),
                count(*requested as u64)
            ),
            Self::IdsTooLarge => tr("body identifiers too large for the computation engine").to_owned(),
            Self::BufferTooLarge { device, buffer_mb, limit_mb } => tr!(
                "too many bodies for this graphics card ({}: buffer of {} MB, limit {} MB)",
                device,
                count(*buffer_mb),
                count(*limit_mb)
            ),
        }
    }
}

impl Localized for FormatError {
    fn localized(&self) -> String {
        let text = match self {
            Self::Truncated => tr("truncated file"),
            Self::NotASave => tr("missing signature: this is not a Stellar save"),
            Self::NotARecording => tr("this is not a Stellar recording"),
            Self::OversizedHeader => tr("oversized header"),
            Self::InconsistentBodyArrays => tr("inconsistent body arrays"),
            Self::TruncatedSnapshot => tr("truncated snapshot"),
            Self::InvalidVarint => tr("invalid varint"),
            Self::InconsistentBodyCount => tr("inconsistent body count"),
            Self::InvalidSubsystemCount => tr("invalid subsystem count"),
            Self::UnknownSubsystem => tr("unknown subsystem"),
            Self::UnknownFate => tr("unknown fate"),
            Self::SnapshotOutOfRange => tr("snapshot out of range"),
            Self::MissingSnapshot(step) => {
                return tr!("the snapshot of step {} is missing from the recording", step)
            }
            Self::EmptyRecording => tr("empty recording"),
            Self::Decode(e) => return e.clone(),
        };
        text.to_owned()
    }
}

impl Localized for RepositoryError {
    fn localized(&self) -> String {
        match self {
            Self::Io(e) => tr!("input/output error: {}", e),
            Self::NotFound(path) => tr!("file not found: {}", path.display()),
            Self::NameTaken(name) => tr!("the name “{}” is already taken", name),
            Self::DeleteFailed(code) => tr!("deletion cancelled or impossible (code {})", code),
            Self::RecorderInterrupted => tr("recording thread interrupted").to_owned(),
            Self::RecordingClosed => tr("recording already closed").to_owned(),
            Self::Format(e) => tr!("invalid file: {}", e.localized()),
            Self::UnsupportedVersion(v) => tr!("unsupported format version: {}", v),
        }
    }
}

impl Localized for ContinuationError {
    fn localized(&self) -> String {
        match self {
            Self::Repository(e) => e.localized(),
            Self::OtherSimulation => {
                tr("the recording comes from another simulation (different initial conditions)").to_owned()
            }
            Self::StartsAfterSave => tr("the recording starts after the save's step").to_owned(),
            Self::MissingBody { id, step } => tr!(
                "body no. {} of the save is missing from the recording at step {}: it comes from another simulation",
                id,
                step
            ),
        }
    }
}

impl Localized for AppError {
    fn localized(&self) -> String {
        match self {
            Self::UnknownEngine(id) => tr!("unknown computation engine: “{}”", id),
            Self::NotReady => tr("the simulation is not ready yet").to_owned(),
            Self::EngineFailed(None) => tr("the computation engine could not start").to_owned(),
            Self::EngineFailed(Some(e)) => tr!("the computation engine could not start: {}", e.localized()),
            Self::Repository(e) => e.localized(),
            Self::Continuation(e) => e.localized(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{apply, Lang};

    #[test]
    fn nested_errors_are_translated() {
        apply(Lang::En);
        let e = AppError::Repository(RepositoryError::Format(FormatError::Truncated));
        assert_eq!(e.localized(), "invalid file: truncated file");
        let e = AppError::EngineFailed(Some(EngineError::TooManyBodies { requested: 20_000_000, limit: 16_776_960 }));
        assert_eq!(
            e.localized(),
            "the computation engine could not start: the computation engine is limited to 16,776,960 bodies (20,000,000 requested)"
        );
        assert_eq!(RepositoryError::Io("Access denied".into()).localized(), "input/output error: Access denied");
        apply(Lang::Fr);
        assert_eq!(
            RepositoryError::Format(FormatError::InvalidVarint).localized(),
            "fichier invalide : varint invalide"
        );
    }
}

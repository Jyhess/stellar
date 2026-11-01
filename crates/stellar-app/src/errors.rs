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
            Self::NoDevice => tr("aucune carte graphique compatible n'a été trouvée").to_owned(),
            Self::ComputeUnsupported => tr("la carte graphique ne gère pas les shaders de calcul").to_owned(),
            Self::DeviceOpen(e) => tr!("ouverture de la carte graphique impossible : {}", e),
            Self::TooManyBodies { requested, limit } => tr!(
                "le moteur de calcul est limité à {} corps ({} demandés)",
                count(*limit as u64),
                count(*requested as u64)
            ),
            Self::IdsTooLarge => tr("identifiants de corps trop grands pour le moteur de calcul").to_owned(),
            Self::BufferTooLarge { device, buffer_mb, limit_mb } => tr!(
                "trop de corps pour cette carte graphique ({} : tampon de {} Mo, limite {} Mo)",
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
            Self::Truncated => tr("fichier tronqué"),
            Self::NotASave => tr("signature absente : ce n'est pas une sauvegarde Stellar"),
            Self::NotARecording => tr("ce n'est pas un enregistrement Stellar"),
            Self::OversizedHeader => tr("en-tête démesuré"),
            Self::InconsistentBodyArrays => tr("tableaux de corps incohérents"),
            Self::TruncatedSnapshot => tr("instantané tronqué"),
            Self::InvalidVarint => tr("varint invalide"),
            Self::InconsistentBodyCount => tr("nombre de corps incohérent"),
            Self::InvalidSubsystemCount => tr("nombre de sous-systèmes invalide"),
            Self::UnknownSubsystem => tr("sous-système inconnu"),
            Self::UnknownFate => tr("devenir inconnu"),
            Self::SnapshotOutOfRange => tr("instantané hors limites"),
            Self::MissingSnapshot(step) => {
                return tr!("l'instantané du pas {} est introuvable dans l'enregistrement", step)
            }
            Self::EmptyRecording => tr("enregistrement vide"),
            Self::Decode(e) => return e.clone(),
        };
        text.to_owned()
    }
}

impl Localized for RepositoryError {
    fn localized(&self) -> String {
        match self {
            Self::Io(e) => tr!("erreur d'entrée/sortie : {}", e),
            Self::NotFound(path) => tr!("fichier introuvable : {}", path.display()),
            Self::NameTaken(name) => tr!("le nom « {} » est déjà pris", name),
            Self::DeleteFailed(code) => tr!("suppression annulée ou impossible (code {})", code),
            Self::RecorderInterrupted => tr("thread d'enregistrement interrompu").to_owned(),
            Self::RecordingClosed => tr("enregistrement déjà clos").to_owned(),
            Self::Format(e) => tr!("fichier invalide : {}", e.localized()),
            Self::UnsupportedVersion(v) => tr!("version de format non prise en charge : {}", v),
        }
    }
}

impl Localized for ContinuationError {
    fn localized(&self) -> String {
        match self {
            Self::Repository(e) => e.localized(),
            Self::OtherSimulation => {
                tr("l'enregistrement provient d'une autre simulation (conditions initiales différentes)").to_owned()
            }
            Self::StartsAfterSave => tr("l'enregistrement commence après le pas de la sauvegarde").to_owned(),
            Self::MissingBody { id, step } => tr!(
                "le corps n° {} de la sauvegarde est absent de l'enregistrement au pas {} : il provient d'une autre simulation",
                id,
                step
            ),
        }
    }
}

impl Localized for AppError {
    fn localized(&self) -> String {
        match self {
            Self::UnknownEngine(id) => tr!("moteur de calcul inconnu : « {} »", id),
            Self::NotReady => tr("la simulation n'est pas encore prête").to_owned(),
            Self::EngineFailed(None) => tr("le moteur de calcul n'a pas pu démarrer").to_owned(),
            Self::EngineFailed(Some(e)) => tr!("le moteur de calcul n'a pas pu démarrer : {}", e.localized()),
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

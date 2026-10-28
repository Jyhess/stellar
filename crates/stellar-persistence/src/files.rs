//! Opérations sur les fichiers communes aux sauvegardes et aux
//! enregistrements : suppression (corbeille de Windows au besoin) et
//! renommage sans écrasement.

use std::fs;
use std::path::{Path, PathBuf};

use stellar_application::RepositoryError;

/// Nom de fichier sûr : lettres, chiffres, tirets et soulignés ; le reste
/// devient un souligné.
pub fn sanitize(name: &str, fallback: &str) -> String {
    let s: String = name
        .trim()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == ',' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let s = s.trim_matches('_').to_owned();
    if s.is_empty() {
        fallback.to_owned()
    } else {
        s
    }
}

/// Supprime un fichier : vers la corbeille si `trash` (Windows), sinon
/// définitivement.
pub fn delete(path: &Path, trash: bool) -> Result<(), RepositoryError> {
    if !path.exists() {
        return Err(RepositoryError::Io(format!(
            "fichier introuvable : {}",
            path.display()
        )));
    }
    if trash {
        return to_trash(path);
    }
    fs::remove_file(path).map_err(|e| RepositoryError::Io(e.to_string()))
}

/// Renomme `path` en `name` (même dossier, même extension) ; refuse
/// d'écraser un fichier existant.
pub fn rename(path: &Path, name: &str, extension: &str, fallback: &str) -> Result<PathBuf, RepositoryError> {
    let target = path.with_file_name(format!("{}.{extension}", sanitize(name, fallback)));
    if target == path {
        return Ok(target);
    }
    if target.exists() {
        return Err(RepositoryError::Io(format!(
            "le nom « {} » est déjà pris",
            target.file_stem().unwrap_or_default().to_string_lossy()
        )));
    }
    fs::rename(path, &target).map_err(|e| RepositoryError::Io(e.to_string()))?;
    Ok(target)
}

#[cfg(windows)]
fn to_trash(path: &Path) -> Result<(), RepositoryError> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::Shell::{
        SHFileOperationW, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_WANTNUKEWARNING,
        FO_DELETE, SHFILEOPSTRUCTW,
    };
    let absolute = fs::canonicalize(path).map_err(|e| RepositoryError::Io(e.to_string()))?;
    // Chemin sans le préfixe \\?\ de canonicalize, terminé par deux zéros.
    let text = absolute.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut wide: Vec<u16> = std::ffi::OsStr::new(text).encode_wide().collect();
    wide.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: wide.as_ptr(),
        pTo: std::ptr::null(),
        // Corbeille ; avertissement si le fichier est trop gros pour elle
        // (il serait sinon supprimé définitivement sans prévenir).
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_WANTNUKEWARNING) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY : chaînes terminées par deux zéros, vivantes pendant l'appel.
    let code = unsafe { SHFileOperationW(&mut op) };
    if code != 0 || op.fAnyOperationsAborted != 0 {
        return Err(RepositoryError::Io(format!(
            "suppression annulée ou impossible (code {code})"
        )));
    }
    Ok(())
}

#[cfg(not(windows))]
fn to_trash(path: &Path) -> Result<(), RepositoryError> {
    fs::remove_file(path).map_err(|e| RepositoryError::Io(e.to_string()))
}

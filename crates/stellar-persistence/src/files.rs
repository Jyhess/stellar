//! File operations shared by saves and recordings: deletion (to the Windows
//! recycle bin when asked) and renaming without overwriting.

use std::fs;
use std::path::{Path, PathBuf};

use stellar_application::RepositoryError;

/// Safe file name: letters, digits, hyphens and underscores; anything else
/// becomes an underscore.
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

/// Deletes a file: to the recycle bin if `trash` (Windows), otherwise
/// permanently.
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

/// Renames `path` to `name` (same folder, same extension); refuses to
/// overwrite an existing file.
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
    // Path without the \\?\ prefix added by canonicalize, terminated by two zeros.
    let text = absolute.to_string_lossy();
    let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let mut wide: Vec<u16> = std::ffi::OsStr::new(text).encode_wide().collect();
    wide.extend([0, 0]);
    let mut op = SHFILEOPSTRUCTW {
        hwnd: std::ptr::null_mut(),
        wFunc: FO_DELETE,
        pFrom: wide.as_ptr(),
        pTo: std::ptr::null(),
        // Recycle bin; warning if the file is too large for it (it would
        // otherwise be deleted permanently without notice).
        fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_WANTNUKEWARNING) as u16,
        fAnyOperationsAborted: 0,
        hNameMappings: std::ptr::null_mut(),
        lpszProgressTitle: std::ptr::null(),
    };
    // SAFETY: strings terminated by two zeros, alive during the call.
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

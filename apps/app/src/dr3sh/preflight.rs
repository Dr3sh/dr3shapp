use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use crate::dr3sh::download::{drive_root_for_path, format_bytes};
use crate::dr3sh::errors::Dr3shError;
use crate::dr3sh::logger::log_event;

const WINDOWS_RESERVED_NAMES: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

pub fn sanitize_instance_folder_name(raw: &str) -> Result<String, Dr3shError> {
    let mut cleaned = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(
            c,
            '/' | '\\' | '?' | '*' | ':' | '\'' | '"' | '|' | '<' | '>' | '!'
        ) || c.is_control()
        {
            continue;
        }
        cleaned.push(c);
    }

    let trimmed = cleaned.trim().trim_end_matches(|c| c == '.' || c == ' ').trim();
    if trimmed.is_empty() {
        return Err(Dr3shError::PreflightNotWritable {
            path: raw.to_string(),
            os_error: "El nombre de carpeta quedó vacío tras sanear caracteres inválidos".to_string(),
        });
    }

    let stem = trimmed.split('.').next().unwrap_or(trimmed).to_ascii_uppercase();
    if WINDOWS_RESERVED_NAMES.contains(&stem.as_str()) {
        return Err(Dr3shError::PreflightNotWritable {
            path: raw.to_string(),
            os_error: format!("Nombre reservado de Windows no permitido: {}", stem),
        });
    }

    Ok(trimmed.to_string())
}

pub fn check_writable(dir: &Path) -> Result<(), Dr3shError> {
    fs::create_dir_all(dir).map_err(|e| Dr3shError::PreflightNotWritable {
        path: dir.to_string_lossy().to_string(),
        os_error: e.to_string(),
    })?;

    let test_file = dir.join(".dr3sh_write_test");
    {
        let mut f = File::create(&test_file).map_err(|e| Dr3shError::PreflightNotWritable {
            path: dir.to_string_lossy().to_string(),
            os_error: e.to_string(),
        })?;
        f.write_all(b"ok").map_err(|e| Dr3shError::PreflightNotWritable {
            path: dir.to_string_lossy().to_string(),
            os_error: e.to_string(),
        })?;
        let _ = f.sync_all();
    }

    fs::remove_file(&test_file).map_err(|e| Dr3shError::PreflightNotWritable {
        path: dir.to_string_lossy().to_string(),
        os_error: e.to_string(),
    })?;

    Ok(())
}

#[cfg(target_os = "windows")]
pub fn get_volume_filesystem(path: &Path) -> Result<String, String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetVolumeInformationW;

    let root = drive_root_for_path(path);
    let mut root_str = root.to_string_lossy().to_string();
    if !root_str.ends_with('\\') {
        root_str.push('\\');
    }
    let wide_root: Vec<u16> = std::ffi::OsStr::new(&root_str)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut fs_name_buf = [0u16; 64];
    let ok = unsafe {
        GetVolumeInformationW(
            PCWSTR(wide_root.as_ptr()),
            None,
            None,
            None,
            None,
            Some(&mut fs_name_buf),
        )
    };

    if let Err(e) = ok {
        return Err(format!("GetVolumeInformationW failed for {}: {}", root_str, e));
    }

    let len = fs_name_buf
        .iter()
        .position(|&c| c == 0)
        .unwrap_or(fs_name_buf.len());
    Ok(String::from_utf16_lossy(&fs_name_buf[..len]))
}

#[cfg(not(target_os = "windows"))]
pub fn get_volume_filesystem(_path: &Path) -> Result<String, String> {
    Ok("NTFS".to_string())
}

#[cfg(target_os = "windows")]
pub fn get_free_disk_space(path: &Path) -> Result<(u64, String), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let root = drive_root_for_path(path);
    let mut root_str = root.to_string_lossy().to_string();
    if !root_str.ends_with('\\') {
        root_str.push('\\');
    }
    let wide_root: Vec<u16> = std::ffi::OsStr::new(&root_str)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_available: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            PCWSTR(wide_root.as_ptr()),
            Some(&mut free_available),
            None,
            None,
        )
    };

    if let Err(e) = ok {
        return Err(format!("GetDiskFreeSpaceExW failed for {}: {}", root_str, e));
    }

    Ok((free_available, root_str.trim_end_matches('\\').to_string()))
}

#[cfg(not(target_os = "windows"))]
pub fn get_free_disk_space(path: &Path) -> Result<(u64, String), String> {
    Ok((100 * 1024 * 1024 * 1024, path.to_string_lossy().to_string()))
}

#[derive(Debug, Clone)]
pub struct PreflightReport {
    pub instance_fs: String,
    pub custom_fs: Option<String>,
    pub free_bytes: u64,
    pub required_bytes: u64,
    pub drive: String,
}

pub fn run_preflight(
    op_id: &str,
    default_instance_dir: &Path,
    custom_instance_dir: Option<&Path>,
    zip_remaining_bytes: u64,
    write_bytes: u64,
) -> Result<PreflightReport, Dr3shError> {
    let effective_dir = custom_instance_dir.unwrap_or(default_instance_dir);
    let needs_junction = custom_instance_dir
        .map(|custom| !paths_equal_case_insensitive(custom, default_instance_dir))
        .unwrap_or(false);

    // Check filesystem types BEFORE creating directories or modifying anything on disk
    let default_fs = get_volume_filesystem(default_instance_dir).unwrap_or_else(|_| "UNKNOWN".to_string());
    let custom_fs = custom_instance_dir
        .map(|c| get_volume_filesystem(c).unwrap_or_else(|_| "UNKNOWN".to_string()));

    if needs_junction {
        if !default_fs.eq_ignore_ascii_case("NTFS") {
            return Err(Dr3shError::JunctionUnsupportedFs {
                fs: default_fs,
                path: default_instance_dir.to_string_lossy().to_string(),
            });
        }
        if let (Some(c_dir), Some(c_fs)) = (custom_instance_dir, &custom_fs) {
            if !c_fs.eq_ignore_ascii_case("NTFS") {
                return Err(Dr3shError::JunctionUnsupportedFs {
                    fs: c_fs.clone(),
                    path: c_dir.to_string_lossy().to_string(),
                });
            }
        }
    }

    // Check free disk space BEFORE writing anything
    let base_bytes = zip_remaining_bytes.saturating_add(write_bytes);
    let plus_ten_pct = ((base_bytes as f64) * 1.10) as u64;
    let required_bytes = plus_ten_pct.saturating_add(200 * 1024 * 1024);

    let (free_bytes, drive) = get_free_disk_space(effective_dir).unwrap_or((u64::MAX, "C:".to_string()));
    if free_bytes < required_bytes {
        return Err(Dr3shError::PreflightNoSpace {
            free: format_bytes(free_bytes),
            required: format_bytes(required_bytes),
            drive,
            path: effective_dir.to_string_lossy().to_string(),
        });
    }

    // Writability test on default parent and custom target
    if let Some(parent) = default_instance_dir.parent() {
        check_writable(parent)?;
    }
    check_writable(effective_dir)?;

    let report = PreflightReport {
        instance_fs: custom_fs.clone().unwrap_or_else(|| default_fs.clone()),
        custom_fs,
        free_bytes,
        required_bytes,
        drive: drive.clone(),
    };

    log_event(
        Some(op_id),
        "PREFLIGHT",
        "INFO",
        "PREFLIGHT OK",
        &[
            ("instance_dir", effective_dir.to_string_lossy().to_string()),
            ("default_dir", default_instance_dir.to_string_lossy().to_string()),
            ("needs_junction", needs_junction.to_string()),
            ("fs", report.instance_fs.clone()),
            ("drive", drive),
            ("free_bytes", free_bytes.to_string()),
            ("required_bytes", required_bytes.to_string()),
        ],
    );

    Ok(report)
}

pub fn paths_equal_case_insensitive(a: &Path, b: &Path) -> bool {
    let na = a.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    let nb = b.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_ascii_lowercase();
    na == nb
}

pub fn is_reparse_point(path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0400;
        (meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT) != 0
    }
    #[cfg(not(target_os = "windows"))]
    {
        false
    }
}

#[cfg(target_os = "windows")]
pub fn create_ntfs_junction(link: &Path, target: &Path) -> Result<(), Dr3shError> {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    use windows::Win32::System::IO::DeviceIoControl;

    const FSCTL_SET_REPARSE_POINT: u32 = 0x000900A4;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA0000003;

    let abs_target = if target.is_absolute() {
        target.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(target)
    };
    let clean_target = abs_target
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();

    let substitute_name = format!(r"\??\{}", clean_target);
    let print_name = clean_target.clone();

    let sub_utf16: Vec<u16> = std::ffi::OsStr::new(&substitute_name).encode_wide().collect();
    let print_utf16: Vec<u16> = std::ffi::OsStr::new(&print_name).encode_wide().collect();

    let sub_byte_len = (sub_utf16.len() * 2) as u16;
    let print_byte_len = (print_utf16.len() * 2) as u16;
    let print_byte_offset = sub_byte_len + 2;
    let reparse_data_len = 8u16 + sub_byte_len + 2 + print_byte_len + 2;
    let total_buf_len = 8usize + reparse_data_len as usize;

    let mut buffer = vec![0u8; total_buf_len];
    buffer[0..4].copy_from_slice(&IO_REPARSE_TAG_MOUNT_POINT.to_le_bytes());
    buffer[4..6].copy_from_slice(&reparse_data_len.to_le_bytes());
    buffer[6..8].copy_from_slice(&0u16.to_le_bytes());
    buffer[8..10].copy_from_slice(&0u16.to_le_bytes());
    buffer[10..12].copy_from_slice(&sub_byte_len.to_le_bytes());
    buffer[12..14].copy_from_slice(&print_byte_offset.to_le_bytes());
    buffer[14..16].copy_from_slice(&print_byte_len.to_le_bytes());

    let mut cursor = 16usize;
    for unit in &sub_utf16 {
        buffer[cursor..cursor + 2].copy_from_slice(&unit.to_le_bytes());
        cursor += 2;
    }
    cursor += 2; // null terminator for SubstituteName
    for unit in &print_utf16 {
        buffer[cursor..cursor + 2].copy_from_slice(&unit.to_le_bytes());
        cursor += 2;
    }

    fs::create_dir_all(link).map_err(|e| Dr3shError::FsPermission {
        path: link.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let dir_file = fs::OpenOptions::new()
        .write(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(link)
        .map_err(|e| {
            let _ = fs::remove_dir(link);
            Dr3shError::FsPermission {
                path: link.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            }
        })?;

    let handle = HANDLE(dir_file.as_raw_handle());
    let mut bytes_returned = 0u32;
    let io_res = unsafe {
        DeviceIoControl(
            handle,
            FSCTL_SET_REPARSE_POINT,
            Some(buffer.as_ptr() as *const std::ffi::c_void),
            buffer.len() as u32,
            None,
            0,
            Some(&mut bytes_returned),
            None,
        )
    };
    drop(dir_file);

    if let Err(e) = io_res {
        let _ = fs::remove_dir(link);
        return Err(Dr3shError::FsPermission {
            path: link.to_string_lossy().to_string(),
            os_code: e.code().0.to_string(),
        });
    }

    let read_back = fs::read_link(link).map_err(|e| {
        let _ = fs::remove_dir(link);
        Dr3shError::FsPermission {
            path: link.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        }
    })?;

    let clean_read = read_back
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_string();
    if !paths_equal_case_insensitive(Path::new(&clean_read), Path::new(&clean_target)) {
        let _ = fs::remove_dir(link);
        return Err(Dr3shError::unknown(format!(
            "Junction verification mismatch: expected {}, got {}",
            clean_target, clean_read
        )));
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
pub fn create_ntfs_junction(link: &Path, target: &Path) -> Result<(), Dr3shError> {
    std::os::unix::fs::symlink(target, link).map_err(|e| Dr3shError::FsPermission {
        path: link.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })
}

fn count_files_and_bytes(dir: &Path) -> Result<(u64, u64), std::io::Error> {
    let mut files = 0u64;
    let mut bytes = 0u64;
    if !dir.exists() {
        return Ok((0, 0));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() && !is_reparse_point(&path) {
            let (f, b) = count_files_and_bytes(&path)?;
            files += f;
            bytes += b;
        } else if meta.is_file() {
            files += 1;
            bytes += meta.len();
        }
    }
    Ok((files, bytes))
}

fn copy_dir_recursive_verified(src: &Path, dst: &Path) -> Result<(), std::io::Error> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let path = entry.path();
        let dest_path = dst.join(entry.file_name());
        let meta = fs::symlink_metadata(&path)?;
        if meta.is_dir() && !is_reparse_point(&path) {
            copy_dir_recursive_verified(&path, &dest_path)?;
        } else if meta.is_file() {
            fs::copy(&path, &dest_path)?;
            let copied_len = fs::metadata(&dest_path)?.len();
            if copied_len != meta.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "Size mismatch after copying {}: expected {}, got {}",
                        path.display(),
                        meta.len(),
                        copied_len
                    ),
                ));
            }
        } else if meta.file_type().is_symlink() && !dest_path.exists() {
            if let Ok(target_meta) = fs::metadata(&path) {
                if target_meta.is_file() {
                    let _ = fs::copy(&path, &dest_path);
                }
            }
        }
    }
    Ok(())
}

pub fn is_launcher_owned_dir(dir: &Path) -> bool {
    dir.join(".dr3sh").exists()
        || dir.join(".dr3sh_instance").exists()
        || dir.join("profile.json").exists()
}

pub fn remove_junction_link_only(link: &Path) -> Result<(), Dr3shError> {
    if !is_reparse_point(link) {
        return Err(Dr3shError::unknown(format!(
            "Refusing to remove non-junction directory as link: {}",
            link.display()
        )));
    }
    fs::remove_dir(link).map_err(|e| Dr3shError::FsPermission {
        path: link.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })
}

pub fn ensure_safe_junction(
    op_id: Option<&str>,
    link_path: &Path,
    target_path: &Path,
) -> Result<(), Dr3shError> {
    if paths_equal_case_insensitive(link_path, target_path) {
        fs::create_dir_all(target_path).map_err(|e| Dr3shError::FsPermission {
            path: target_path.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
        return Ok(());
    }

    // Check NTFS requirement before doing anything
    let link_fs = get_volume_filesystem(link_path).unwrap_or_else(|_| "UNKNOWN".to_string());
    if !link_fs.eq_ignore_ascii_case("NTFS") {
        return Err(Dr3shError::JunctionUnsupportedFs {
            fs: link_fs,
            path: link_path.to_string_lossy().to_string(),
        });
    }
    let target_fs = get_volume_filesystem(target_path).unwrap_or_else(|_| "UNKNOWN".to_string());
    if !target_fs.eq_ignore_ascii_case("NTFS") {
        return Err(Dr3shError::JunctionUnsupportedFs {
            fs: target_fs,
            path: target_path.to_string_lossy().to_string(),
        });
    }

    fs::create_dir_all(target_path).map_err(|e| Dr3shError::FsPermission {
        path: target_path.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    let marker = target_path.join(".dr3sh_instance");
    if !marker.exists() {
        let _ = fs::write(&marker, b"dr3sh");
    }

    // If link_path is already a junction/reparse point:
    if is_reparse_point(link_path) {
        if let Ok(existing_target) = fs::read_link(link_path) {
            let clean_existing = existing_target
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_string();
            let clean_desired = target_path
                .to_string_lossy()
                .trim_start_matches(r"\\?\")
                .to_string();
            if paths_equal_case_insensitive(Path::new(&clean_existing), Path::new(&clean_desired)) {
                return Ok(());
            }
        }
        // Remove ONLY the junction link, never its target
        remove_junction_link_only(link_path)?;
    }

    // If link_path does not exist or is an empty directory:
    if !link_path.exists() {
        if let Some(parent) = link_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        create_ntfs_junction(link_path, target_path)?;
        log_event(
            op_id,
            "JUNCTION",
            "INFO",
            "created junction for non-existing link path",
            &[
                ("link", link_path.to_string_lossy().to_string()),
                ("target", target_path.to_string_lossy().to_string()),
            ],
        );
        return Ok(());
    }

    let (src_files, src_bytes) = count_files_and_bytes(link_path).map_err(|e| {
        Dr3shError::FsPermission {
            path: link_path.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        }
    })?;

    // Copy any existing files/structure into target_path, verify counts, then atomically swap via .dr3sh_old
    copy_dir_recursive_verified(link_path, target_path).map_err(|e| Dr3shError::FsPermission {
        path: target_path.to_string_lossy().to_string(),
        os_code: e.raw_os_error().unwrap_or(-1).to_string(),
    })?;

    if src_files > 0 {
        let (after_files, after_bytes) = count_files_and_bytes(target_path).map_err(|e| {
            Dr3shError::FsPermission {
                path: target_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            }
        })?;

        if after_files < src_files || after_bytes < src_bytes {
            return Err(Dr3shError::unknown(format!(
                "Cross-volume copy verification failed: src=({} files, {} bytes), dst=({} files, {} bytes)",
                src_files, src_bytes, after_files, after_bytes
            )));
        }
    }

    let folder_name = link_path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("instance");
    let old_backup_path = link_path.with_file_name(format!("{}.dr3sh_old", folder_name));
    if old_backup_path.exists() && is_launcher_owned_dir(&old_backup_path) {
        let _ = fs::remove_dir_all(&old_backup_path);
    }

    let mut renamed = false;
    for _ in 0..10 {
        if fs::rename(link_path, &old_backup_path).is_ok() {
            renamed = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    if !renamed {
        copy_dir_recursive_verified(link_path, &old_backup_path).map_err(|e| {
            Dr3shError::FsPermission {
                path: old_backup_path.to_string_lossy().to_string(),
                os_code: e.raw_os_error().unwrap_or(-1).to_string(),
            }
        })?;
        fs::remove_dir_all(link_path).map_err(|e| Dr3shError::FsPermission {
            path: link_path.to_string_lossy().to_string(),
            os_code: e.raw_os_error().unwrap_or(-1).to_string(),
        })?;
    }

    if let Err(e) = create_ntfs_junction(link_path, target_path) {
        // Restore original from .dr3sh_old
        if fs::rename(&old_backup_path, link_path).is_err() {
            let _ = copy_dir_recursive_verified(&old_backup_path, link_path);
        }
        return Err(e);
    }

    // Verify junction with read_link before deleting .dr3sh_old
    if fs::read_link(link_path).is_ok() {
        let _ = fs::write(old_backup_path.join(".dr3sh_instance"), b"dr3sh_migrated");
        if is_launcher_owned_dir(&old_backup_path) {
            let _ = fs::remove_dir_all(&old_backup_path);
        }
    }

    log_event(
        op_id,
        "JUNCTION",
        "INFO",
        "migrated directory via verified copy and created junction",
        &[
            ("link", link_path.to_string_lossy().to_string()),
            ("target", target_path.to_string_lossy().to_string()),
            ("files", src_files.to_string()),
            ("bytes", src_bytes.to_string()),
        ],
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_folder_name() {
        assert_eq!(
            sanitize_instance_folder_name("Cobbleverse: v1.4! .. ").unwrap(),
            "Cobbleverse v1.4"
        );
        assert!(sanitize_instance_folder_name("CON").is_err());
        assert!(sanitize_instance_folder_name("nul.txt").is_err());
        assert!(sanitize_instance_folder_name("com1").is_err());
        assert!(sanitize_instance_folder_name("???...").is_err());
    }

    #[test]
    fn test_native_junction_create_and_safe_remove() {
        let base = std::env::temp_dir().join(format!("dr3sh_junc_test_{}", uuid::Uuid::new_v4()));
        let link = base.join("link_dir");
        let target = base.join("target_dir");
        fs::create_dir_all(&link).unwrap();
        fs::write(link.join("hello.txt"), b"preserve_me").unwrap();

        ensure_safe_junction(Some("test_junc"), &link, &target).unwrap();

        assert!(is_reparse_point(&link));
        assert!(fs::read_link(&link).is_ok());
        assert_eq!(fs::read(target.join("hello.txt")).unwrap(), b"preserve_me");
        assert_eq!(fs::read(link.join("hello.txt")).unwrap(), b"preserve_me");

        // Removing the junction link only must leave the target intact
        remove_junction_link_only(&link).unwrap();
        assert!(!link.exists());
        assert_eq!(fs::read(target.join("hello.txt")).unwrap(), b"preserve_me");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_sim_junction_with_theseus_empty_subdirs_and_preflight_no_space() {
        let base = std::env::temp_dir().join(format!("dr3sh_junc_theseus_{}", uuid::Uuid::new_v4()));
        let link = base.join("profiles/Desgraciamon");
        let target = base.join("custom/Desgraciamon");

        // Simulate Theseus creating empty subdirectories before junction creation (os_code=145 scenario)
        for sub in ["mods", "config", "resourcepacks", "shaderpacks", "saves", "screenshots", "crash-reports", "datapacks"] {
            fs::create_dir_all(link.join(sub)).unwrap();
        }

        ensure_safe_junction(Some("sim_junc_theseus"), &link, &target).unwrap();
        assert!(is_reparse_point(&link));
        assert!(target.join("mods").is_dir());

        // Idempotent second call when junction already points to target
        ensure_safe_junction(Some("sim_junc_theseus_2"), &link, &target).unwrap();
        assert!(is_reparse_point(&link));

        // Preflight insufficient space simulation (requesting u64::MAX / 2 bytes)
        let err = run_preflight(
            "sim_space",
            &link,
            Some(&target),
            u64::MAX / 4,
            u64::MAX / 4,
        )
        .unwrap_err();
        assert_eq!(err.code(), "E_PREFLIGHT_NO_SPACE");

        remove_junction_link_only(&link).unwrap();
        let _ = fs::remove_dir_all(&base);
    }
}

use crate::api::Result;
use std::{
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
};
use url::Url;
use windows::{
    Win32::{
        Foundation::{PROPERTYKEY, RPC_E_CHANGED_MODE},
        System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
            COINIT_DISABLE_OLE1DDE, CoCreateInstance, CoInitializeEx,
            CoUninitialize, IPersistFile, StructuredStorage::PROPVARIANT,
        },
        UI::Shell::{
            IShellLinkW, PropertiesSystem::IPropertyStore, SHCNE_ASSOCCHANGED,
            SHCNE_CREATE, SHCNE_UPDATEDIR, SHCNE_UPDATEITEM, SHCNF_IDLIST,
            SHCNF_PATHW, SHChangeNotify, ShellLink,
        },
    },
    core::{GUID, Interface, PCWSTR},
};

const PKEY_APP_USER_MODEL_ID: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3),
    pid: 5,
};

pub(super) const SHORTCUT_EXTENSION: &str = "lnk";

fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    let raw = path.to_string_lossy();
    if let Some(stripped) = raw.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = raw.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path
    }
}

pub(super) async fn create_shortcut(
    profile_name: &str,
    launch_url: &Url,
    output_path: &Path,
    icon_path: Option<&Path>,
) -> Result<()> {
    let target_path = strip_unc_prefix(std::env::current_exe()?);
    let working_dir = target_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let output_path = strip_unc_prefix(output_path.to_path_buf());
    let launch_url = launch_url.to_string();
    let profile_name = profile_name.to_string();
    let icon_path = icon_path.map(|p| strip_unc_prefix(p.to_path_buf()));

    // Run on a dedicated fresh OS thread so COM can always initialize as STA
    // (Tokio's blocking pool threads may already be initialized as MTA).
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("dr3sh-shortcut".to_string())
        .spawn(move || {
            let res = create_windows_shortcut(
                output_path,
                target_path,
                working_dir,
                launch_url,
                profile_name,
                icon_path,
            );
            let _ = tx.send(res);
        })
        .map_err(|e| anyhow::anyhow!("No se pudo iniciar el hilo de acceso directo: {e}"))?;

    let inner = rx
        .await
        .map_err(|_| anyhow::anyhow!("Se interrumpió la creación del acceso directo"))?;
    inner?;

    Ok(())
}

fn create_windows_shortcut(
    output_path: PathBuf,
    target_path: PathBuf,
    working_dir: PathBuf,
    launch_url: String,
    profile_name: String,
    icon_path: Option<PathBuf>,
) -> std::io::Result<()> {
    if let Some(parent) = output_path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)?;
    }

    let icon_file = icon_path
        .filter(|p| p.is_file())
        .unwrap_or_else(|| target_path.clone());

    let output_wide = windows_wide_path(&output_path);
    let target_wide = windows_wide_path(&target_path);
    let icon_wide = windows_wide_path(&icon_file);
    let working_wide = windows_wide_path(&working_dir);
    let launch_url_wide = windows_wide_string(&launch_url);
    let description_wide = windows_wide_string(&format!("Jugar {profile_name}"));

    unsafe {
        let init_result = CoInitializeEx(
            None,
            COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE,
        );
        let should_uninit = if init_result == RPC_E_CHANGED_MODE {
            false
        } else {
            windows_result(init_result.ok())?;
            true
        };
        let _com = WindowsComGuard { should_uninit };

        let shortcut: IShellLinkW = windows_result(CoCreateInstance(
            &ShellLink,
            None,
            CLSCTX_INPROC_SERVER,
        ))?;
        windows_result(shortcut.SetPath(windows_pcwstr(&target_wide)))?;
        windows_result(shortcut.SetArguments(windows_pcwstr(&launch_url_wide)))?;
        windows_result(
            shortcut.SetWorkingDirectory(windows_pcwstr(&working_wide)),
        )?;
        windows_result(
            shortcut.SetIconLocation(windows_pcwstr(&icon_wide), 0),
        )?;
        windows_result(
            shortcut.SetDescription(windows_pcwstr(&description_wide)),
        )?;

        let safe_id: String = profile_name
            .chars()
            .filter_map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    Some(c.to_ascii_lowercase())
                } else if c.is_whitespace() {
                    Some('_')
                } else {
                    None
                }
            })
            .take(40)
            .collect();
        let safe_id = if safe_id.is_empty() {
            "instance".to_string()
        } else {
            safe_id
        };
        let app_user_model_id = format!("com.dr3sh.launcher.shortcut.{safe_id}");
        if let Ok(prop_store) = shortcut.cast::<IPropertyStore>() {
            let prop_value = PROPVARIANT::from(app_user_model_id.as_str());
            let _ = prop_store.SetValue(&PKEY_APP_USER_MODEL_ID, &prop_value);
            let _ = prop_store.Commit();
        }

        let persist_file: IPersistFile = windows_result(shortcut.cast())?;
        windows_result(persist_file.Save(windows_pcwstr(&output_wide), true))?;
    }

    if !output_path.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!(
                "Windows no escribió el archivo de acceso directo en {}",
                output_path.display()
            ),
        ));
    }

    unsafe {
        SHChangeNotify(
            SHCNE_CREATE,
            SHCNF_PATHW,
            Some(output_wide.as_ptr() as *const _),
            None,
        );
        SHChangeNotify(
            SHCNE_UPDATEITEM,
            SHCNF_PATHW,
            Some(output_wide.as_ptr() as *const _),
            None,
        );
        if let Some(parent) = output_path.parent()
            && !parent.as_os_str().is_empty()
        {
            let parent_wide = windows_wide_path(parent);
            SHChangeNotify(
                SHCNE_UPDATEDIR,
                SHCNF_PATHW,
                Some(parent_wide.as_ptr() as *const _),
                None,
            );
        }
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }

    Ok(())
}

fn windows_result<T>(result: windows::core::Result<T>) -> std::io::Result<T> {
    result.map_err(std::io::Error::other)
}

struct WindowsComGuard {
    should_uninit: bool,
}

impl Drop for WindowsComGuard {
    fn drop(&mut self) {
        if self.should_uninit {
            unsafe {
                CoUninitialize();
            }
        }
    }
}

fn windows_wide_path(path: &Path) -> Vec<u16> {
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn windows_wide_string(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn windows_pcwstr(value: &[u16]) -> PCWSTR {
    PCWSTR::from_raw(value.as_ptr())
}

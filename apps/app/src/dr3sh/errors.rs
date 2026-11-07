use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dr3shErrorPayload {
    pub code: String,
    pub op_id: String,
    pub stage: String,
    pub message: String,
    pub details: String,
    pub i18n_key: String,
    pub i18n_values: serde_json::Value,
}

#[derive(Debug, Clone)]
pub enum Dr3shError {
    PreflightNoSpace {
        drive: String,
        required: String,
        free: String,
        path: String,
    },
    PreflightNotWritable {
        path: String,
        os_error: String,
    },
    JunctionUnsupportedFs {
        fs: String,
        path: String,
    },
    ManifestFetch {
        status: String,
        host: String,
    },
    ManifestInvalid {
        reason: String,
    },
    Network {
        last_cause: String,
        retries: u32,
        offset: u64,
    },
    HttpNotFound {
        status: u16,
        url: String,
    },
    HttpForbidden {
        status: u16,
        host: String,
    },
    HtmlInsteadOfZip {
        content_type: String,
        snippet: String,
        host: String,
    },
    SizeMismatch {
        expected: u64,
        actual: u64,
    },
    HashMismatch {
        expected: String,
        actual: String,
    },
    ZipCorrupt {
        entry: String,
        error: String,
    },
    ZipContentMismatch {
        missing_entries: Vec<String>,
    },
    ZipUnsafePath {
        entry: String,
    },
    FsLocked {
        path: String,
        os_code: String,
    },
    FsPermission {
        path: String,
        os_code: String,
    },
    TheseusBase {
        theseus_error: String,
    },
    TheseusTimeout {
        last_progress: String,
    },
    RollbackFailed {
        path: String,
        failed_op: String,
    },
    Cancelled {
        stage: String,
    },
    Unknown {
        op_id: String,
        cause_chain: String,
    },
}

impl Dr3shError {
    pub fn unknown(cause_chain: impl Into<String>) -> Self {
        Self::Unknown {
            op_id: "0000".to_string(),
            cause_chain: cause_chain.into(),
        }
    }

    pub fn ui_message_es(&self) -> String {
        self.ui_message("0000")
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::PreflightNoSpace { .. } => "E_PREFLIGHT_NO_SPACE",
            Self::PreflightNotWritable { .. } => "E_PREFLIGHT_NOT_WRITABLE",
            Self::JunctionUnsupportedFs { .. } => "E_JUNCTION_UNSUPPORTED_FS",
            Self::ManifestFetch { .. } => "E_MANIFEST_FETCH",
            Self::ManifestInvalid { .. } => "E_MANIFEST_INVALID",
            Self::Network { .. } => "E_NETWORK",
            Self::HttpNotFound { .. } => "E_HTTP_NOT_FOUND",
            Self::HttpForbidden { .. } => "E_HTTP_FORBIDDEN",
            Self::HtmlInsteadOfZip { .. } => "E_HTML_INSTEAD_OF_ZIP",
            Self::SizeMismatch { .. } => "E_SIZE_MISMATCH",
            Self::HashMismatch { .. } => "E_HASH_MISMATCH",
            Self::ZipCorrupt { .. } => "E_ZIP_CORRUPT",
            Self::ZipContentMismatch { .. } => "E_ZIP_CONTENT_MISMATCH",
            Self::ZipUnsafePath { .. } => "E_ZIP_UNSAFE_PATH",
            Self::FsLocked { .. } => "E_FS_LOCKED",
            Self::FsPermission { .. } => "E_FS_PERMISSION",
            Self::TheseusBase { .. } => "E_THESEUS_BASE",
            Self::TheseusTimeout { .. } => "E_THESEUS_TIMEOUT",
            Self::RollbackFailed { .. } => "E_ROLLBACK_FAILED",
            Self::Cancelled { .. } => "E_CANCELLED",
            Self::Unknown { .. } => "E_UNKNOWN",
        }
    }

    pub fn i18n_key(&self) -> &'static str {
        match self {
            Self::PreflightNoSpace { .. } => "dr3sh.error.preflight_no_space",
            Self::PreflightNotWritable { .. } => "dr3sh.error.preflight_not_writable",
            Self::JunctionUnsupportedFs { .. } => "dr3sh.error.junction_unsupported_fs",
            Self::ManifestFetch { .. } => "dr3sh.error.manifest_fetch",
            Self::ManifestInvalid { .. } => "dr3sh.error.manifest_invalid",
            Self::Network { .. } => "dr3sh.error.network",
            Self::HttpNotFound { .. } => "dr3sh.error.http_not_found",
            Self::HttpForbidden { .. } => "dr3sh.error.http_forbidden",
            Self::HtmlInsteadOfZip { .. } => "dr3sh.error.html_instead_of_zip",
            Self::SizeMismatch { .. } => "dr3sh.error.size_mismatch",
            Self::HashMismatch { .. } => "dr3sh.error.hash_mismatch",
            Self::ZipCorrupt { .. } => "dr3sh.error.zip_corrupt",
            Self::ZipContentMismatch { .. } => "dr3sh.error.zip_content_mismatch",
            Self::ZipUnsafePath { .. } => "dr3sh.error.zip_unsafe_path",
            Self::FsLocked { .. } => "dr3sh.error.fs_locked",
            Self::FsPermission { .. } => "dr3sh.error.fs_permission",
            Self::TheseusBase { .. } => "dr3sh.error.theseus_base",
            Self::TheseusTimeout { .. } => "dr3sh.error.theseus_timeout",
            Self::RollbackFailed { .. } => "dr3sh.error.rollback_failed",
            Self::Cancelled { .. } => "dr3sh.error.cancelled",
            Self::Unknown { .. } => "dr3sh.error.unknown",
        }
    }

    pub fn i18n_values(&self, op_id: &str) -> serde_json::Value {
        match self {
            Self::PreflightNoSpace {
                drive, required, ..
            } => serde_json::json!({ "drive": drive, "required": required }),
            Self::JunctionUnsupportedFs { fs, .. } => serde_json::json!({ "fs": fs }),
            Self::RollbackFailed { path, .. } => serde_json::json!({ "path": path }),
            Self::Unknown { .. } => serde_json::json!({ "op_id": op_id }),
            _ => serde_json::json!({}),
        }
    }

    pub fn ui_message(&self, op_id: &str) -> String {
        match self {
            Self::PreflightNoSpace {
                drive, required, ..
            } => format!("Necesitas {required} libres en {drive}."),
            Self::PreflightNotWritable { .. } => {
                "No se puede escribir en la carpeta seleccionada.".to_string()
            }
            Self::JunctionUnsupportedFs { fs, .. } => {
                format!("El disco usa {fs}; elige un disco NTFS o la carpeta por defecto.")
            }
            Self::ManifestFetch { .. } => {
                "No se pudo obtener la información del modpack.".to_string()
            }
            Self::ManifestInvalid { .. } => "La información del modpack es inválida.".to_string(),
            Self::Network { .. } => {
                "Conexión inestable. No se pudo completar la descarga.".to_string()
            }
            Self::HttpNotFound { .. } => {
                "El archivo del modpack no existe o fue movido.".to_string()
            }
            Self::HttpForbidden { .. } => "El servidor rechazó la descarga.".to_string(),
            Self::HtmlInsteadOfZip { .. } => {
                "El servidor devolvió una página web en lugar del archivo.".to_string()
            }
            Self::SizeMismatch { .. } => "El tamaño del archivo no coincide.".to_string(),
            Self::HashMismatch { .. } => {
                "El archivo llegó dañado. Inténtalo de nuevo.".to_string()
            }
            Self::ZipCorrupt { .. } => "El paquete del modpack está dañado.".to_string(),
            Self::ZipContentMismatch { .. } => {
                "El paquete no coincide con su manifiesto.".to_string()
            }
            Self::ZipUnsafePath { .. } => {
                "El paquete contiene rutas no permitidas.".to_string()
            }
            Self::FsLocked { .. } => {
                "Otro programa (¿antivirus?) está bloqueando un archivo.".to_string()
            }
            Self::FsPermission { .. } => "Permiso denegado al escribir archivos.".to_string(),
            Self::TheseusBase { .. } => {
                "Falló la instalación base de Minecraft/loader.".to_string()
            }
            Self::TheseusTimeout { .. } => "La instalación base no avanza.".to_string(),
            Self::RollbackFailed { path, .. } => {
                format!("No se pudo restaurar la instancia. Copia de seguridad en {path}.")
            }
            Self::Cancelled { .. } => "Operación cancelada.".to_string(),
            Self::Unknown { .. } => format!("Error inesperado (código {op_id})."),
        }
    }

    pub fn logged_details(&self) -> Vec<(String, String)> {
        match self {
            Self::PreflightNoSpace {
                drive,
                required,
                free,
                path,
            } => vec![
                ("drive".to_string(), drive.clone()),
                ("free".to_string(), free.clone()),
                ("required".to_string(), required.clone()),
                ("path".to_string(), path.clone()),
            ],
            Self::PreflightNotWritable { path, os_error } => vec![
                ("path".to_string(), path.clone()),
                ("os_error".to_string(), os_error.clone()),
            ],
            Self::JunctionUnsupportedFs { fs, path } => vec![
                ("fs".to_string(), fs.clone()),
                ("path".to_string(), path.clone()),
            ],
            Self::ManifestFetch { status, host } => vec![
                ("status".to_string(), status.clone()),
                ("host".to_string(), host.clone()),
            ],
            Self::ManifestInvalid { reason } => vec![("reason".to_string(), reason.clone())],
            Self::Network {
                last_cause,
                retries,
                offset,
            } => vec![
                ("last_cause".to_string(), last_cause.clone()),
                ("retries".to_string(), retries.to_string()),
                ("offset".to_string(), offset.to_string()),
            ],
            Self::HttpNotFound { status, url } => vec![
                ("status".to_string(), status.to_string()),
                ("url".to_string(), url.clone()),
            ],
            Self::HttpForbidden { status, host } => vec![
                ("status".to_string(), status.to_string()),
                ("host".to_string(), host.clone()),
            ],
            Self::HtmlInsteadOfZip {
                content_type,
                snippet,
                host,
            } => vec![
                ("content_type".to_string(), content_type.clone()),
                ("host".to_string(), host.clone()),
                ("first_2kb".to_string(), snippet.clone()),
            ],
            Self::SizeMismatch { expected, actual } => vec![
                ("expected".to_string(), expected.to_string()),
                ("actual".to_string(), actual.to_string()),
            ],
            Self::HashMismatch { expected, actual } => vec![
                ("expected".to_string(), expected.clone()),
                ("actual".to_string(), actual.clone()),
            ],
            Self::ZipCorrupt { entry, error } => vec![
                ("entry".to_string(), entry.clone()),
                ("error".to_string(), error.clone()),
            ],
            Self::ZipContentMismatch { missing_entries } => vec![(
                "missing_entries".to_string(),
                missing_entries.join(", "),
            )],
            Self::ZipUnsafePath { entry } => vec![("entry".to_string(), entry.clone())],
            Self::FsLocked { path, os_code } => vec![
                ("path".to_string(), path.clone()),
                ("os_code".to_string(), os_code.clone()),
            ],
            Self::FsPermission { path, os_code } => vec![
                ("path".to_string(), path.clone()),
                ("os_code".to_string(), os_code.clone()),
            ],
            Self::TheseusBase { theseus_error } => {
                vec![("theseus_error".to_string(), theseus_error.clone())]
            }
            Self::TheseusTimeout { last_progress } => {
                vec![("last_progress".to_string(), last_progress.clone())]
            }
            Self::RollbackFailed { path, failed_op } => vec![
                ("path".to_string(), path.clone()),
                ("failed_op".to_string(), failed_op.clone()),
            ],
            Self::Cancelled { stage } => vec![("stage".to_string(), stage.clone())],
            Self::Unknown { op_id, cause_chain } => vec![
                ("op_id".to_string(), op_id.clone()),
                ("cause_chain".to_string(), cause_chain.clone()),
            ],
        }
    }

    pub fn details_summary(&self) -> String {
        self.logged_details()
            .into_iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(" ")
    }

    pub fn to_payload(&self, op_id: &str, stage: &str) -> Dr3shErrorPayload {
        Dr3shErrorPayload {
            code: self.code().to_string(),
            op_id: op_id.to_string(),
            stage: stage.to_string(),
            message: self.ui_message(op_id),
            details: self.details_summary(),
            i18n_key: self.i18n_key().to_string(),
            i18n_values: self.i18n_values(op_id),
        }
    }
}

impl fmt::Display for Dr3shError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let op_id = match self {
            Self::Unknown { op_id, .. } => op_id.as_str(),
            _ => "0000",
        };
        write!(
            f,
            "[{}] {} ({})",
            self.code(),
            self.ui_message(op_id),
            self.details_summary()
        )
    }
}

impl std::error::Error for Dr3shError {}

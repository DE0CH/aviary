//! The extension table (magpie 10.3, supervisor 13): a file's MIME type from its name, the same everywhere a type
//! is guessed (pigeon's upload without a part type, the supervisor's sent-file snapshot, magpie's file routes).

/// The MIME type of a file named `name`, from its extension (case-insensitive); `application/octet-stream` when the
/// extension is unknown or absent.
#[must_use]
pub fn from_name(name: &str) -> &'static str {
    let base = name.rsplit('/').next().unwrap_or(name);
    let ext = base
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "application/pdf",
        "txt" | "log" => "text/plain",
        "md" => "text/markdown",
        "csv" => "text/csv",
        "tsv" => "text/tab-separated-values",
        "json" => "application/json",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "zip" => "application/zip",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "avif" => "image/avif",
        "tif" | "tiff" => "image/tiff",
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "mp3" => "audio/mpeg",
        "mp4" => "video/mp4",
        "mov" => "video/quicktime",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table() {
        assert_eq!(
            from_name("Report.XLSX"),
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        );
        assert_eq!(from_name("/workspace/out/chart.PNG"), "image/png");
        assert_eq!(from_name("x"), "application/octet-stream");
        assert_eq!(from_name("dir.d/x"), "application/octet-stream");
        assert_eq!(from_name("scan.tiff"), "image/tiff");
    }
}

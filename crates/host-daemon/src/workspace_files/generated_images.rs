use super::*;
use base64::engine::general_purpose::STANDARD;

const IMAGE_BYTES: u64 = 16 * 1024 * 1024;

impl WorkspaceFiles {
    /// Native image bytes are files, never conversation or event payloads.
    pub(crate) fn persist_generated_image(
        &self,
        thread: &str,
        item: &str,
        data: &str,
    ) -> Result<String> {
        let data = if let Some(url) = data.strip_prefix("data:") {
            let (header, data) = url
                .split_once(',')
                .ok_or_else(|| anyhow!("invalid image data URL"))?;
            if !header.starts_with("image/") || !header.ends_with(";base64") {
                return Err(anyhow!("generated image data URL is not a base64 image"));
            }
            data
        } else {
            data
        };
        if data.len() as u64 > IMAGE_BYTES.div_ceil(3) * 4 {
            return Err(anyhow!("generated image exceeds 16 MiB"));
        }
        let bytes = STANDARD
            .decode(data)
            .context("cannot decode generated image")?;
        if bytes.len() as u64 > IMAGE_BYTES {
            return Err(anyhow!("generated image exceeds 16 MiB"));
        }
        let extension =
            match image::guess_format(&bytes).context("unknown generated image format")? {
                image::ImageFormat::Png => "png",
                image::ImageFormat::Jpeg => "jpg",
                image::ImageFormat::Gif => "gif",
                image::ImageFormat::WebP => "webp",
                _ => return Err(anyhow!("unsupported generated image format")),
            };
        let hash = digest::digest(&SHA256, &bytes);
        let key = serde_json::to_vec(&(thread, item, hash.as_ref()))?;
        let id = uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_URL, &key);
        let path =
            self.persist_asset(&format!("generated-{id}.{extension}"), &bytes, IMAGE_BYTES)?;
        path.to_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow!("generated image path is not UTF-8"))
    }
}

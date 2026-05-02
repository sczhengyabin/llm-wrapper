//! 图片下载与 base64 转换

use anyhow::{Context, Result};
use base64::Engine;
use reqwest::Client;
use std::net::IpAddr;

pub struct ImageDownloader {
    client: Client,
}

impl ImageDownloader {
    pub fn new() -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("build image download client"),
        }
    }

    /// 下载图片 URL 并转为 base64
    ///
    /// SSRF 防护：拒绝 loopback/private/link-local IP
    /// 大小限制：20MB
    /// 允许 MIME 类型：image/jpeg, image/png, image/gif, image/webp
    pub async fn download_to_base64(&self, url: &str) -> Result<(String, String)> {
        let parsed = url.parse::<reqwest::Url>().context("invalid image URL")?;

        assert_allowed_scheme(&parsed)?;
        assert_not_private_ip(&parsed)?;

        let response = self.client.get(url).send().await?;

        let content_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        assert_allowed_image_type(&content_type)?;

        let bytes = response.bytes().await?;
        assert_size_limit(&bytes, 20 * 1024 * 1024)?;

        let media_type = content_type;
        let base64_data = base64::engine::general_purpose::STANDARD.encode(&bytes);

        Ok((base64_data, media_type))
    }
}

fn assert_allowed_scheme(url: &reqwest::Url) -> Result<()> {
    match url.scheme() {
        "http" | "https" => Ok(()),
        other => Err(anyhow::anyhow!(
            "Unsupported URL scheme: {} (only http/https allowed)",
            other
        )),
    }
}

fn assert_not_private_ip(url: &reqwest::Url) -> Result<()> {
    if let Some(host) = url.host_str() {
        if let Ok(ip) = host.parse::<IpAddr>() {
            let is_private = match ip {
                IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
                IpAddr::V6(v6) => v6.is_loopback(),
            };
            if is_private {
                return Err(anyhow::anyhow!(
                    "Refusing to download image from private IP: {}",
                    ip
                ));
            }
        }
    }
    Ok(())
}

fn assert_allowed_image_type(content_type: &str) -> Result<()> {
    match content_type {
        "image/jpeg" | "image/png" | "image/gif" | "image/webp" => Ok(()),
        other if other.starts_with("image/") => Err(anyhow::anyhow!(
            "Unsupported image type: {} (allowed: jpeg, png, gif, webp)",
            other
        )),
        _ => Err(anyhow::anyhow!(
            "Not an image: content_type={}",
            content_type
        )),
    }
}

fn assert_size_limit(bytes: &[u8], max: usize) -> Result<()> {
    if bytes.len() > max {
        Err(anyhow::anyhow!(
            "Image exceeds size limit: {} bytes (max {} bytes)",
            bytes.len(),
            max
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowed_schemes() {
        assert!(assert_allowed_scheme(&"https://example.com/img.png".parse().unwrap()).is_ok());
        assert!(assert_allowed_scheme(&"http://example.com/img.png".parse().unwrap()).is_ok());
        assert!(assert_allowed_scheme(&"file:///local.png".parse().unwrap()).is_err());
        assert!(assert_allowed_scheme(&"ftp://server/img.png".parse().unwrap()).is_err());
    }

    #[test]
    fn test_private_ip_rejected() {
        assert!(assert_not_private_ip(&"http://127.0.0.1/img.png".parse().unwrap()).is_err());
        assert!(assert_not_private_ip(&"http://192.168.1.1/img.png".parse().unwrap()).is_err());
        assert!(assert_not_private_ip(&"http://10.0.0.1/img.png".parse().unwrap()).is_err());
        assert!(assert_not_private_ip(&"http://169.254.1.1/img.png".parse().unwrap()).is_err());
        assert!(assert_not_private_ip(&"http://93.184.216.34/img.png".parse().unwrap()).is_ok());
        assert!(assert_not_private_ip(&"http://example.com/img.png".parse().unwrap()).is_ok());
    }

    #[test]
    fn test_allowed_image_types() {
        assert!(assert_allowed_image_type("image/jpeg").is_ok());
        assert!(assert_allowed_image_type("image/png").is_ok());
        assert!(assert_allowed_image_type("image/gif").is_ok());
        assert!(assert_allowed_image_type("image/webp").is_ok());
        assert!(assert_allowed_image_type("image/svg+xml").is_err());
        assert!(assert_allowed_image_type("text/html").is_err());
    }

    #[test]
    fn test_size_limit() {
        let small = b"small";
        assert!(assert_size_limit(small, 1024).is_ok());

        let large: Vec<u8> = vec![0u8; 21 * 1024 * 1024];
        assert!(assert_size_limit(&large, 20 * 1024 * 1024).is_err());
    }
}

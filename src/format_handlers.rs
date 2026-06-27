use anyhow::{Context, Result};
use image::{
    codecs::bmp::BmpEncoder,
    codecs::ico::IcoEncoder,
    codecs::png::PngEncoder,
    codecs::tiff::TiffEncoder,
    DynamicImage, ImageEncoder,
};
use turbojpeg::{compress as tj_compress, Image as TjImage, PixelFormat as TjPixelFormat, Subsamp};
use std::io::{BufWriter, Cursor};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ImageFormat {
    Jpg,
    Png,
    WebP,
    Bmp,
    Gif,
    Tiff,
    Ico,
    Heif,
    Heic,
    Jxl,
}

impl ImageFormat {
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "jpg" | "jpeg" => Some(ImageFormat::Jpg),
            "png" => Some(ImageFormat::Png),
            "webp" => Some(ImageFormat::WebP),
            "bmp" => Some(ImageFormat::Bmp),
            "gif" => Some(ImageFormat::Gif),
            "tiff" | "tif" => Some(ImageFormat::Tiff),
            "ico" => Some(ImageFormat::Ico),
            "heif" => Some(ImageFormat::Heif),
            "heic" => Some(ImageFormat::Heic),
            "jxl" => Some(ImageFormat::Jxl),
            _ => None,
        }
    }

    pub fn to_string(&self) -> &'static str {
        match self {
            ImageFormat::Jpg => "jpg",
            ImageFormat::Png => "png",
            ImageFormat::WebP => "webp",
            ImageFormat::Bmp => "bmp",
            ImageFormat::Gif => "gif",
            ImageFormat::Tiff => "tiff",
            ImageFormat::Ico => "ico",
            ImageFormat::Heif => "heif",
            ImageFormat::Heic => "heic",
            ImageFormat::Jxl => "jxl",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            ImageFormat::Jpg => "image/jpeg",
            ImageFormat::Png => "image/png",
            ImageFormat::WebP => "image/webp",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Tiff => "image/tiff",
            ImageFormat::Ico => "image/x-icon",
            ImageFormat::Heif => "image/heif",
            ImageFormat::Heic => "image/heic",
            ImageFormat::Jxl => "image/jxl",
        }
    }

    pub fn extension(&self) -> &'static str {
        self.to_string()
    }
}

pub struct FormatHandler;

impl FormatHandler {
    pub fn encode_to_buffer(
        img: &DynamicImage,
        format: ImageFormat,
        quality: u8,
    ) -> Result<Vec<u8>> {
        let mut buffer = Vec::new();
        {
            let mut writer = BufWriter::new(&mut buffer);

            match format {
                ImageFormat::Jpg => {
                    return encode_jpeg_turbo(img, quality);
                }
                ImageFormat::Png => {
                    let encoder = PngEncoder::new_with_quality(
                        &mut writer,
                        image::codecs::png::CompressionType::Fast,
                        image::codecs::png::FilterType::Adaptive,
                    );
                    encoder
                        .write_image(
                            img.as_bytes(),
                            img.width(),
                            img.height(),
                            img.color().into(),
                        )
                        .context("Failed to encode PNG")?;
                }
                ImageFormat::Bmp => {
                    let encoder = BmpEncoder::new(&mut writer);
                    encoder
                        .write_image(
                            img.as_bytes(),
                            img.width(),
                            img.height(),
                            img.color().into(),
                        )
                        .context("Failed to encode BMP")?;
                }
                ImageFormat::Tiff => {
                    let mut cursor = Cursor::new(Vec::new());
                    let encoder = TiffEncoder::new(&mut cursor);
                    encoder
                        .write_image(
                            img.as_bytes(),
                            img.width(),
                            img.height(),
                            img.color().into(),
                        )
                        .context("Failed to encode TIFF")?;
                    return Ok(cursor.into_inner());
                }
                ImageFormat::Ico => {
                    let encoder = IcoEncoder::new(&mut writer);
                    encoder
                        .write_image(
                            img.as_bytes(),
                            img.width(),
                            img.height(),
                            img.color().into(),
                        )
                        .context("Failed to encode ICO")?;
                }
                ImageFormat::WebP => {
                    return encode_webp(img, quality);
                }
                _ => {
                    return Err(anyhow::anyhow!("Format not yet implemented: {:?}", format));
                }
            }
        }

        Ok(buffer)
    }

    pub fn detect_format_from_path(path: &Path) -> Option<ImageFormat> {
        let ext = path.extension()?.to_str()?;
        ImageFormat::from_str(ext)
    }

    pub fn get_supported_formats() -> &'static [&'static str] {
        &[
            "jpg", "jpeg", "png", "webp", "bmp", "gif", "tiff", "ico", "heif", "heic", "jxl",
        ]
    }
}

fn encode_jpeg_turbo(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgb = img.to_rgb8();
    let width = rgb.width() as usize;
    let height = rgb.height() as usize;
    let pitch = width * 3;
    let pixels = rgb.as_raw().as_slice();
    let image = TjImage {
        pixels,
        width,
        pitch,
        height,
        format: TjPixelFormat::RGB,
    };

    let quality = quality.clamp(1, 100) as i32;
    let buf = tj_compress(image, quality, Subsamp::Sub2x2)
        .context("Failed to encode JPEG (turbojpeg)")?;
    Ok(buf.to_vec())
}

pub fn encode_jpeg_from_rgb(
    pixels: &[u8],
    width: u32,
    height: u32,
    quality: u8,
) -> Result<Vec<u8>> {
    let pitch = width as usize * 3;
    let image = TjImage {
        pixels,
        width: width as usize,
        pitch,
        height: height as usize,
        format: TjPixelFormat::RGB,
    };
    let quality = quality.clamp(1, 100) as i32;
    let buf = tj_compress(image, quality, Subsamp::Sub2x2)
        .context("Failed to encode JPEG (turbojpeg)")?;
    Ok(buf.to_vec())
}

fn encode_webp(img: &DynamicImage, quality: u8) -> Result<Vec<u8>> {
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    let encoder = webp::Encoder::from_rgba(rgba.as_raw(), width, height);

    let webp = if quality >= 100 {
        encoder.encode_lossless()
    } else {
        encoder.encode(quality as f32)
    };

    Ok(webp.as_ref().to_vec())
}

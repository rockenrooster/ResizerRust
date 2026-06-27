use crate::file_handler::{
    ensure_output_dir, extract_date_taken, generate_output_path, set_file_timestamp,
};
use crate::format_handlers::{encode_jpeg_from_rgb, FormatHandler, ImageFormat};
use crate::settings::{ResizeFilter, ResizeMode};
use anyhow::{Context, Result};
use fast_image_resize as fir;
use image::{imageops::FilterType, DynamicImage, RgbImage, RgbaImage};
use std::cell::RefCell;
use std::fs;
use std::path::Path;
use turbojpeg::{Decompressor as TjDecompressor, Image as TjImage, PixelFormat as TjPixelFormat};

#[derive(Clone)]
pub struct ImageProcessorConfig {
    pub output_path: std::path::PathBuf,
    pub quality: u8,
    pub resolution_percent: u32,
    pub threads: usize,
    pub format: ImageFormat,
    pub preserve_structure: bool,
    pub resize_mode: ResizeMode,
    pub max_width: u32,
    pub max_height: u32,
    pub preserve_aspect: bool,
    pub resize_filter: ResizeFilter,
}

#[derive(Debug)]
pub struct ProcessResult {
    pub input_size: u64,
    pub output_size: u64,
}

struct JpegBuffers {
    decode: Vec<u8>,
    resize: Vec<u8>,
}

impl JpegBuffers {
    fn new() -> Self {
        Self {
            decode: Vec::new(),
            resize: Vec::new(),
        }
    }
}

thread_local! {
    static JPEG_BUFFERS: RefCell<JpegBuffers> = RefCell::new(JpegBuffers::new());
}

pub fn process_file(
    queued_file: &crate::file_handler::QueuedFile,
    config: &ImageProcessorConfig,
) -> Result<ProcessResult> {
    let input_path = &queued_file.input_path;
    let relative_path = &queued_file.relative_path;

    let input_size = std::fs::metadata(input_path)?.len();

    if matches!(FormatHandler::detect_format_from_path(input_path), Some(ImageFormat::Jpg)) {
        if let Ok(result) = process_jpeg_fast(input_path, relative_path, config, input_size) {
            return Ok(result);
        }
    }

    let img = image::open(input_path)
        .with_context(|| format!("Failed to load image {:?}", input_path))?;

    let orig_w = img.width();
    let orig_h = img.height();
    let resized_img = match config.resize_mode {
        ResizeMode::Percent => {
            let percent = config.resolution_percent.clamp(1, 100);
            if percent == 100 {
                img
            } else {
                let new_width = (orig_w * percent) / 100;
                let new_height = (orig_h * percent) / 100;
                resize_with_fallback(img, new_width.max(1), new_height.max(1), config.resize_filter)
            }
        }
        ResizeMode::Max => {
            let max_w = config.max_width.max(1);
            let max_h = config.max_height.max(1);
            if config.preserve_aspect {
                let (new_w, new_h) = fit_within(orig_w, orig_h, max_w, max_h);
                if new_w == orig_w && new_h == orig_h {
                    img
                } else {
                    resize_with_fallback(img, new_w, new_h, config.resize_filter)
                }
            } else {
                let new_w = orig_w.min(max_w).max(1);
                let new_h = orig_h.min(max_h).max(1);
                if new_w == orig_w && new_h == orig_h {
                    img
                } else {
                    resize_exact_with_fallback(img, new_w, new_h, config.resize_filter)
                }
            }
        }
    };

    let resized_img = if config.format == ImageFormat::Ico {
        let ico_max = 256;
        if resized_img.width() > ico_max || resized_img.height() > ico_max {
            let (new_w, new_h) = fit_within(
                resized_img.width(),
                resized_img.height(),
                ico_max,
                ico_max,
            );
            resize_with_fallback(resized_img, new_w, new_h, ResizeFilter::Lanczos3)
        } else {
            resized_img
        }
    } else {
        resized_img
    };

    let output_path = generate_output_path(
        input_path,
        relative_path,
        &config.output_path,
        config.format.to_string(),
        config.preserve_structure,
    )?;

    ensure_output_dir(&output_path)?;

    match config.format {
        ImageFormat::Jpg
        | ImageFormat::Png
        | ImageFormat::Bmp
        | ImageFormat::Tiff
        | ImageFormat::WebP
        | ImageFormat::Ico => {
            write_standard_format(&resized_img, &output_path, config.format, config.quality)?;
        }
        ImageFormat::Gif => {
            write_gif(&resized_img, &output_path)?;
        }
        _ => {
            write_standard_format(
                &resized_img,
                &output_path,
                ImageFormat::Jpg,
                config.quality,
            )
            .with_context(|| {
                format!(
                    "Format {:?} not yet implemented, falling back to JPG",
                    config.format
                )
            })?;
        }
    }

    if let Ok(date_taken) = extract_date_taken(input_path) {
        let _ = set_file_timestamp(&output_path, date_taken);
    }

    let output_size = std::fs::metadata(&output_path)?.len();

    Ok(ProcessResult {
        input_size,
        output_size,
    })
}

fn write_standard_format(
    img: &DynamicImage,
    output_path: &Path,
    format: ImageFormat,
    quality: u8,
) -> Result<()> {
    let buffer = FormatHandler::encode_to_buffer(img, format, quality)?;

    fs::write(output_path, &buffer)
        .with_context(|| format!("Failed to write output file {:?}", output_path))?;

    Ok(())
}

fn write_gif(img: &DynamicImage, output_path: &Path) -> Result<()> {
    img.save(output_path)
        .with_context(|| format!("Failed to save GIF to {:?}", output_path))?;
    Ok(())
}

fn process_jpeg_fast(
    input_path: &Path,
    relative_path: &str,
    config: &ImageProcessorConfig,
    input_size: u64,
) -> Result<ProcessResult> {
    JPEG_BUFFERS.with(|cell| {
        let mut buffers = cell.borrow_mut();
        let jpeg_data = fs::read(input_path)
            .with_context(|| format!("Failed to read JPEG data {:?}", input_path))?;

        let mut decompressor = TjDecompressor::new()
            .with_context(|| "Failed to create TurboJPEG decompressor")?;
        let header = decompressor
            .read_header(&jpeg_data)
            .with_context(|| format!("Failed to read JPEG header {:?}", input_path))?;

        let orig_w = header.width as u32;
        let orig_h = header.height as u32;
        let pitch = header.width * 3;
        let needed = pitch * header.height;
        if buffers.decode.len() < needed {
            buffers.decode.resize(needed, 0);
        }

        let mut image = TjImage {
            pixels: &mut buffers.decode[..needed],
            width: header.width,
            pitch,
            height: header.height,
            format: TjPixelFormat::RGB,
        };
        decompressor
            .decompress(&jpeg_data, image.as_deref_mut())
            .with_context(|| format!("Failed to decode JPEG {:?}", input_path))?;

        let mut width = orig_w;
        let mut height = orig_h;
        let mut source_is_decode = true;
        let mut decode = std::mem::take(&mut buffers.decode);
        let mut resize = std::mem::take(&mut buffers.resize);

        match config.resize_mode {
            ResizeMode::Percent => {
                let percent = config.resolution_percent.clamp(1, 100);
                if percent != 100 {
                    let new_w = (orig_w * percent) / 100;
                    let new_h = (orig_h * percent) / 100;
                    let new_w = new_w.max(1);
                    let new_h = new_h.max(1);
                    if source_is_decode {
                        let src = &decode[..needed];
                        let dst = &mut resize;
                        resize_rgb_with_fir_into(
                            src,
                            width,
                            height,
                            new_w,
                            new_h,
                            config.resize_filter,
                            dst,
                        )?;
                    } else {
                        let src = &resize[..(width as usize * height as usize * 3)];
                        let dst = &mut decode;
                        resize_rgb_with_fir_into(
                            src,
                            width,
                            height,
                            new_w,
                            new_h,
                            config.resize_filter,
                            dst,
                        )?;
                    }
                    source_is_decode = !source_is_decode;
                    width = new_w;
                    height = new_h;
                }
            }
            ResizeMode::Max => {
                let max_w = config.max_width.max(1);
                let max_h = config.max_height.max(1);
                let (new_w, new_h) = if config.preserve_aspect {
                    fit_within(orig_w, orig_h, max_w, max_h)
                } else {
                    (orig_w.min(max_w).max(1), orig_h.min(max_h).max(1))
                };
                if new_w != orig_w || new_h != orig_h {
                    if source_is_decode {
                        let src = &decode[..needed];
                        let dst = &mut resize;
                        resize_rgb_with_fir_into(
                            src,
                            width,
                            height,
                            new_w,
                            new_h,
                            config.resize_filter,
                            dst,
                        )?;
                    } else {
                        let src = &resize[..(width as usize * height as usize * 3)];
                        let dst = &mut decode;
                        resize_rgb_with_fir_into(
                            src,
                            width,
                            height,
                            new_w,
                            new_h,
                            config.resize_filter,
                            dst,
                        )?;
                    }
                    source_is_decode = !source_is_decode;
                    width = new_w;
                    height = new_h;
                }
            }
        }

        if config.format == ImageFormat::Ico && (width > 256 || height > 256) {
            let (new_w, new_h) = fit_within(width, height, 256, 256);
            if source_is_decode {
                let src = &decode[..needed];
                let dst = &mut resize;
                resize_rgb_with_fir_into(
                    src,
                    width,
                    height,
                    new_w,
                    new_h,
                    ResizeFilter::Lanczos3,
                    dst,
                )?;
            } else {
                let src = &resize[..(width as usize * height as usize * 3)];
                let dst = &mut decode;
                resize_rgb_with_fir_into(
                    src,
                    width,
                    height,
                    new_w,
                    new_h,
                    ResizeFilter::Lanczos3,
                    dst,
                )?;
            }
            source_is_decode = !source_is_decode;
            width = new_w;
            height = new_h;
        }

        let output_path = generate_output_path(
            input_path,
            relative_path,
            &config.output_path,
            config.format.to_string(),
            config.preserve_structure,
        )?;

        ensure_output_dir(&output_path)?;

        let output_slice = if source_is_decode {
            &decode[..(width as usize * height as usize * 3)]
        } else {
            &resize[..(width as usize * height as usize * 3)]
        };

        match config.format {
            ImageFormat::Jpg => {
                let buffer = encode_jpeg_from_rgb(output_slice, width, height, config.quality)?;
                fs::write(&output_path, &buffer)
                    .with_context(|| format!("Failed to write output file {:?}", output_path))?;
            }
            ImageFormat::Gif => {
                let rgb = RgbImage::from_raw(width, height, output_slice.to_vec())
                    .context("Failed to create RGB image from JPEG buffer")?;
                let dyn_img = DynamicImage::ImageRgb8(rgb);
                write_gif(&dyn_img, &output_path)?;
            }
            _ => {
                let rgb = RgbImage::from_raw(width, height, output_slice.to_vec())
                    .context("Failed to create RGB image from JPEG buffer")?;
                let dyn_img = DynamicImage::ImageRgb8(rgb);
                write_standard_format(&dyn_img, &output_path, config.format, config.quality)?;
            }
        }

        if let Ok(date_taken) = extract_date_taken(input_path) {
            let _ = set_file_timestamp(&output_path, date_taken);
        }

        let output_size = std::fs::metadata(&output_path)?.len();

        buffers.decode = decode;
        buffers.resize = resize;

        Ok(ProcessResult {
            input_size,
            output_size,
        })
    })
}

fn fit_within(orig_w: u32, orig_h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    let scale_w = max_w as f64 / orig_w as f64;
    let scale_h = max_h as f64 / orig_h as f64;
    let scale = scale_w.min(scale_h).min(1.0);
    let new_w = (orig_w as f64 * scale).round().max(1.0) as u32;
    let new_h = (orig_h as f64 * scale).round().max(1.0) as u32;
    (new_w, new_h)
}

fn resize_with_fallback(
    img: DynamicImage,
    new_w: u32,
    new_h: u32,
    filter: ResizeFilter,
) -> DynamicImage {
    match resize_with_fir(&img, new_w, new_h, filter) {
        Ok(resized) => resized,
        Err(err) => {
            eprintln!("fast_image_resize failed, falling back to image crate: {}", err);
            img.resize(new_w, new_h, map_image_filter(filter))
        }
    }
}

fn resize_exact_with_fallback(
    img: DynamicImage,
    new_w: u32,
    new_h: u32,
    filter: ResizeFilter,
) -> DynamicImage {
    match resize_with_fir_exact(&img, new_w, new_h, filter) {
        Ok(resized) => resized,
        Err(err) => {
            eprintln!("fast_image_resize failed, falling back to image crate: {}", err);
            img.resize_exact(new_w, new_h, map_image_filter(filter))
        }
    }
}

fn resize_with_fir(
    img: &DynamicImage,
    new_w: u32,
    new_h: u32,
    filter: ResizeFilter,
) -> Result<DynamicImage> {
    let (src_buf, has_alpha) = if img.color().has_alpha() {
        (img.to_rgba8().into_raw(), true)
    } else {
        (img.to_rgb8().into_raw(), false)
    };

    let pixel_type = if has_alpha {
        fir::PixelType::U8x4
    } else {
        fir::PixelType::U8x3
    };

    let src_image = fir::images::Image::from_vec_u8(
        img.width(),
        img.height(),
        src_buf,
        pixel_type,
    )?;

    let mut dst_image = fir::images::Image::new(new_w, new_h, pixel_type);
    let resize_alg = map_resize_alg(filter);
    let options = fir::ResizeOptions::new()
        .resize_alg(resize_alg)
        .use_alpha(has_alpha);

    let mut resizer = fir::Resizer::new();
    resizer.resize(&src_image, &mut dst_image, &options)?;

    let dst_buf = dst_image.into_vec();
    if has_alpha {
        let rgba = RgbaImage::from_raw(new_w, new_h, dst_buf)
            .context("Failed to create RGBA image from resized buffer")?;
        Ok(DynamicImage::ImageRgba8(rgba))
    } else {
        let rgb = RgbImage::from_raw(new_w, new_h, dst_buf)
            .context("Failed to create RGB image from resized buffer")?;
        Ok(DynamicImage::ImageRgb8(rgb))
    }
}

fn resize_with_fir_exact(
    img: &DynamicImage,
    new_w: u32,
    new_h: u32,
    filter: ResizeFilter,
) -> Result<DynamicImage> {
    resize_with_fir(img, new_w, new_h, filter)
}

fn resize_rgb_with_fir_into(
    src: &[u8],
    width: u32,
    height: u32,
    new_w: u32,
    new_h: u32,
    filter: ResizeFilter,
    dst: &mut Vec<u8>,
) -> Result<()> {
    let needed = new_w as usize * new_h as usize * 3;
    if dst.len() < needed {
        dst.resize(needed, 0);
    }
    let mut dst_slice = &mut dst[..needed];
    let src_image = fir::images::ImageRef::new(width, height, src, fir::PixelType::U8x3)?;
    let mut dst_image =
        fir::images::Image::from_slice_u8(new_w, new_h, &mut dst_slice, fir::PixelType::U8x3)?;
    let resize_alg = map_resize_alg(filter);
    let options = fir::ResizeOptions::new()
        .resize_alg(resize_alg)
        .use_alpha(false);
    let mut resizer = fir::Resizer::new();
    resizer.resize(&src_image, &mut dst_image, &options)?;
    Ok(())
}

fn map_resize_alg(filter: ResizeFilter) -> fir::ResizeAlg {
    match filter {
        ResizeFilter::Nearest => fir::ResizeAlg::Nearest,
        ResizeFilter::Triangle => fir::ResizeAlg::Convolution(fir::FilterType::Bilinear),
        ResizeFilter::CatmullRom => fir::ResizeAlg::Convolution(fir::FilterType::CatmullRom),
        ResizeFilter::Lanczos3 => fir::ResizeAlg::Convolution(fir::FilterType::Lanczos3),
    }
}

fn map_image_filter(filter: ResizeFilter) -> FilterType {
    match filter {
        ResizeFilter::Nearest => FilterType::Nearest,
        ResizeFilter::Triangle => FilterType::Triangle,
        ResizeFilter::CatmullRom => FilterType::CatmullRom,
        ResizeFilter::Lanczos3 => FilterType::Lanczos3,
    }
}

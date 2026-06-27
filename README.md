# ResizerRust

A high-performance batch image converter written in Rust with a Slint GUI.

## Features

- Multi-format support: JPG, PNG, WebP, BMP, GIF, TIFF, ICO
- Batch processing with configurable thread count
- Drag and drop files and folders
- Resolution scaling (percentage-based)
- Quality settings for each format
- EXIF metadata preservation
- Real-time progress tracking and statistics
- Cross-platform (Windows, Linux, macOS)

## Building

```bash
cargo build --release
```

Or build the local Windows artifact:

```powershell
.\build.ps1
```

## Running

```bash
cargo run --release
```

## GitHub Releases

This repo publishes Windows x64 builds from tags. Configure `origin` to point at
the GitHub repo, then ship an update with:

```powershell
.\release.ps1 -Version 1.0.1
```

GitHub Actions will build `resizerrust.exe`, package it as
`resizerrust-v1.0.1-windows-x64.zip`, and attach it to the GitHub Release.

Release builds check GitHub Releases on startup. If a newer release has
`resizerrust.exe` and `resizerrust.exe.sha256` assets, the app asks before it
downloads anything. If accepted, it verifies the SHA-256, replaces the running
exe, and relaunches. Public updates require a public repo or a
`RESIZERRUST_GITHUB_TOKEN` environment variable with release access.

## Dependencies

### Windows
- Microsoft Visual C++ Redistributable
- No additional dependencies required

### Linux
- Install system dependencies:
  ```bash
  sudo apt-get install libclang-dev pkg-config
  ```

### macOS
- Xcode command line tools

## Phase 1-3 Implementation

This implementation includes:

### Phase 1: Foundation
- ✅ Project initialization with Slint
- ✅ Basic UI layout
- ✅ File drag & drop support (UI ready)
- ✅ Settings load/save functionality

### Phase 2: Core Image Processing
- ✅ Image decoding for basic formats (JPG, PNG, BMP)
- ✅ Image resizing logic
- ✅ Encoding to JPG and PNG
- ✅ Basic format conversion

### Phase 3: Multi-threading
- ✅ Producer-consumer channel implementation
- ✅ Parallel worker threads
- ✅ Cancellation support
- ✅ Progress tracking

## Testing

1. Build the project
2. Drag and drop image files onto the UI
3. Configure output settings
4. Click "Convert" to start processing
5. Monitor progress and statistics

## TODO (Phases 4-6)

- [ ] Phase 4: Extended formats (WebP, HEIF, JXL, custom ICO)
- [ ] Phase 5: Metadata & polish (EXIF, timestamps)
- [ ] Phase 6: Advanced features & optimization
